use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use artwork::{ArtworkKey, DecodedImage};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::{Cast, ObjectExt};

// The byte budget remains authoritative for Rufin's smallest 32px cover while
// still bounding anomalous sub-32px texture/object metadata.
const MAX_TEXTURES: usize = 8_192;
const MAX_TEXTURE_BYTES: usize = 32 * 1024 * 1024;

pub(in crate::shell) struct TextureCache {
    entries: HashMap<ArtworkKey, TextureEntry>,
    live_textures: HashMap<ArtworkKey, LiveTexture>,
    order: BTreeSet<TextureAccess>,
    bytes: usize,
    next_access: u64,
    max_textures: usize,
    max_bytes: usize,
}

#[derive(Clone)]
struct LiveTexture {
    texture: glib::WeakRef<gdk::Texture>,
    bytes: usize,
}

struct TextureEntry {
    texture: gdk::Texture,
    bytes: usize,
    last_used: u64,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct TextureAccess {
    last_used: u64,
    key: ArtworkKey,
}

struct TexturePixels(Arc<DecodedImage>);

impl AsRef<[u8]> for TexturePixels {
    fn as_ref(&self) -> &[u8] {
        self.0.rgba()
    }
}

impl TextureCache {
    fn with_limits(max_textures: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            live_textures: HashMap::new(),
            order: BTreeSet::new(),
            bytes: 0,
            next_access: 0,
            max_textures,
            max_bytes,
        }
    }

    fn get(&mut self, key: &ArtworkKey) -> Option<gdk::Texture> {
        let last_used = self.next_access();
        let (previous_access, texture) = {
            let entry = self.entries.get_mut(key)?;
            let previous_access = TextureAccess {
                last_used: entry.last_used,
                key: key.clone(),
            };
            entry.last_used = last_used;
            (previous_access, entry.texture.clone())
        };
        self.order.remove(&previous_access);
        self.order.insert(TextureAccess {
            last_used,
            key: key.clone(),
        });
        Some(texture)
    }

    fn insert(&mut self, key: ArtworkKey, texture: gdk::Texture, bytes: usize) {
        self.remove(&key);
        self.live_textures.insert(
            key.clone(),
            LiveTexture {
                texture: texture.downgrade(),
                bytes,
            },
        );
        let last_used = self.next_access();
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key.clone(),
            TextureEntry {
                texture,
                bytes,
                last_used,
            },
        );
        self.order.insert(TextureAccess { last_used, key });
        self.evict_to_limits();
        if self.live_textures.len() > self.entries.len().saturating_add(self.max_textures) {
            self.live_textures
                .retain(|_, entry| entry.texture.upgrade().is_some());
        }
    }

    fn get_or_revive(&mut self, key: &ArtworkKey) -> Option<gdk::Texture> {
        if let Some(texture) = self.get(key) {
            return Some(texture);
        }
        let live = self.live_textures.get(key)?.clone();
        let Some(texture) = live.texture.upgrade() else {
            self.live_textures.remove(key);
            return None;
        };
        self.insert(key.clone(), texture.clone(), live.bytes);
        Some(texture)
    }

    fn remove(&mut self, key: &ArtworkKey) -> Option<TextureEntry> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&TextureAccess {
            last_used: entry.last_used,
            key: key.clone(),
        });
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        Some(entry)
    }

    fn next_access(&mut self) -> u64 {
        self.next_access = self.next_access.wrapping_add(1).max(1);
        self.next_access
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > self.max_textures || self.bytes > self.max_bytes {
            let Some(key) = self.order.first().map(|access| access.key.clone()) else {
                break;
            };
            self.remove(&key);
        }
    }

    #[cfg(test)]
    fn assert_consistent(&self) {
        let bytes = self
            .entries
            .values()
            .map(|entry| entry.bytes)
            .sum::<usize>();
        assert_eq!(self.bytes, bytes);
        assert!(self.bytes <= self.max_bytes);
        assert!(self.order.len() <= self.max_textures);
        assert_eq!(self.order.len(), self.entries.len());
        for (key, entry) in &self.entries {
            let access = TextureAccess {
                last_used: entry.last_used,
                key: key.clone(),
            };
            assert!(self.order.contains(&access));
        }
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::with_limits(MAX_TEXTURES, MAX_TEXTURE_BYTES)
    }
}

impl TextureCache {
    pub(super) fn prepared_texture(&mut self, key: &ArtworkKey) -> Option<gdk::Texture> {
        self.get_or_revive(key)
    }

    pub(super) fn texture(&mut self, image: Arc<DecodedImage>) -> Option<gdk::Texture> {
        let identity = image.key().clone();
        if let Some(texture) = self.get_or_revive(&identity) {
            return Some(texture);
        }
        let bytes = image.rgba().len();
        let texture = texture_from_decoded(image)?;
        self.insert(identity, texture.clone(), bytes);
        Some(texture)
    }
}

fn texture_from_decoded(image: Arc<DecodedImage>) -> Option<gdk::Texture> {
    let width = i32::try_from(image.width()).ok()?;
    let height = i32::try_from(image.height()).ok()?;
    let row_stride = usize::try_from(image.row_stride()).ok()?;
    let bytes = glib::Bytes::from_owned(TexturePixels(image));
    Some(
        gdk::MemoryTexture::new(
            width,
            height,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            row_stride,
        )
        .upcast(),
    )
}

#[cfg(test)]
mod tests {
    use gtk::prelude::ObjectType;
    use proptest::prelude::*;

    use super::*;

    fn texture(value: u8) -> gdk::Texture {
        let bytes = glib::Bytes::from_owned([value, value, value, 255]);
        gdk::MemoryTexture::new(1, 1, gdk::MemoryFormat::R8g8b8a8, &bytes, 4).upcast()
    }

    fn keys() -> [ArtworkKey; 24] {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let artwork = artwork::Artwork::new(root.path(), runtime.handle().clone()).unwrap();
        std::array::from_fn(|index| {
            let binding = sources::native_artwork_binding(
                "texture-test",
                &sources::NativeImageRef::new(index.to_string(), None),
            )
            .unwrap();
            artwork
                .prepare_cache_only(artwork::ArtworkRequest::new(
                    artwork::ArtworkBinding::opaque(&binding),
                    32,
                    32,
                ))
                .key
        })
    }

    #[test]
    fn cache_reuses_one_texture_and_evicts_by_owned_bytes() {
        let mut cache = TextureCache::with_limits(3, 8);
        let keys = keys();
        let first = texture(1);
        let second = texture(2);
        cache.insert(keys[1].clone(), first.clone(), 4);
        cache.insert(keys[2].clone(), second.clone(), 4);
        let reused = cache
            .get(&keys[1])
            .expect("the first texture remains cached");
        assert_eq!(reused.as_ptr(), first.as_ptr());

        cache.insert(keys[3].clone(), texture(3), 4);

        assert_eq!(cache.bytes, 8);
        assert!(
            cache.get(&keys[1]).is_some(),
            "the recent texture stays cached"
        );
        assert!(
            cache.get(&keys[2]).is_none(),
            "the older strong entry is evicted"
        );
        let revived = cache
            .get_or_revive(&keys[2])
            .expect("a mounted texture remains interned after LRU eviction");
        assert_eq!(revived.as_ptr(), second.as_ptr());
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn count_limit_does_not_preempt_the_budget_for_real_cover_sizes() {
        let smallest_cover_bytes = 32 * 32 * 4;
        assert!(MAX_TEXTURES * smallest_cover_bytes >= MAX_TEXTURE_BYTES);
    }

    proptest! {
        #[test]
        fn arbitrary_cache_operations_preserve_bounds(
            operations in prop::collection::vec(
                (0u8..6, 0u8..24, 1usize..=24, any::<bool>()),
                1..=96,
            ),
        ) {
            let mut cache = TextureCache::with_limits(16, 64);

            let keys = keys();
            for (operation, value, bytes, _) in operations {
                let key = keys[usize::from(value)].clone();
                match operation {
                    0 | 1 => cache.insert(key, texture(value), bytes),
                    2 => {
                        cache.get(&key);
                    }
                    3 => {
                        cache.remove(&key);
                    }
                    4 => {
                        cache.get(&key);
                    }
                    _ => {
                        cache.get_or_revive(&key);
                    }
                }
                cache.assert_consistent();
            }
        }
    }
}
