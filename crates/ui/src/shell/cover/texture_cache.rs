use std::collections::{BTreeSet, HashMap};
use std::hash::Hash;
use std::sync::Arc;

use artwork::{DecodedImage, DecodedImageIdentity};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::{Cast, ObjectExt};
use library::SourceId;

const MAX_THUMBNAIL_TEXTURES: usize = 20_480;
const MAX_THUMBNAIL_TEXTURE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECENT_LARGE_TEXTURES: usize = 4_096;
const MAX_RECENT_LARGE_TEXTURE_BYTES: usize = 32 * 1024 * 1024;
const MAX_THUMBNAIL_TEXTURE_SIZE: u32 = 96;

pub(in crate::shell) struct TextureCache<K = DecodedImageIdentity> {
    entries: HashMap<K, TextureEntry>,
    live_textures: HashMap<K, LiveTexture>,
    thumbnail_order: BTreeSet<TextureAccess<K>>,
    large_order: BTreeSet<TextureAccess<K>>,
    bytes: usize,
    thumbnail_bytes: usize,
    next_access: u64,
    max_thumbnail_textures: usize,
    max_thumbnail_bytes: usize,
    max_large_textures: usize,
    max_large_bytes: usize,
}

#[derive(Clone)]
struct LiveTexture {
    source_id: SourceId,
    texture: glib::WeakRef<gdk::Texture>,
    bytes: usize,
    class: TextureClass,
}

struct TextureEntry {
    texture: gdk::Texture,
    bytes: usize,
    last_used: u64,
    class: TextureClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextureClass {
    Thumbnail,
    Large,
}

fn texture_class(width: u32, height: u32) -> TextureClass {
    if width.max(height) <= MAX_THUMBNAIL_TEXTURE_SIZE {
        TextureClass::Thumbnail
    } else {
        TextureClass::Large
    }
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct TextureAccess<K> {
    last_used: u64,
    key: K,
}

struct TexturePixels(Arc<DecodedImage>);

impl AsRef<[u8]> for TexturePixels {
    fn as_ref(&self) -> &[u8] {
        self.0.rgba()
    }
}

impl<K> TextureCache<K>
where
    K: Clone + Eq + Hash + Ord,
{
    #[cfg(test)]
    fn with_limits(max_textures: usize, max_bytes: usize) -> Self {
        Self::with_class_limits(max_textures, max_bytes, max_textures, max_bytes)
    }

    fn with_class_limits(
        max_thumbnail_textures: usize,
        max_thumbnail_bytes: usize,
        max_large_textures: usize,
        max_large_bytes: usize,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            live_textures: HashMap::new(),
            thumbnail_order: BTreeSet::new(),
            large_order: BTreeSet::new(),
            bytes: 0,
            thumbnail_bytes: 0,
            next_access: 0,
            max_thumbnail_textures,
            max_thumbnail_bytes,
            max_large_textures,
            max_large_bytes,
        }
    }

    fn get(&mut self, key: &K) -> Option<gdk::Texture> {
        let last_used = self.next_access();
        let (previous_access, class, texture) = {
            let entry = self.entries.get_mut(key)?;
            let previous_access = TextureAccess {
                last_used: entry.last_used,
                key: key.clone(),
            };
            entry.last_used = last_used;
            (previous_access, entry.class, entry.texture.clone())
        };
        self.order_mut(class).remove(&previous_access);
        self.order_mut(class).insert(TextureAccess {
            last_used,
            key: key.clone(),
        });
        Some(texture)
    }

    fn insert_with_class(
        &mut self,
        key: K,
        source_id: SourceId,
        texture: gdk::Texture,
        bytes: usize,
        class: TextureClass,
    ) {
        self.remove(&key);
        self.live_textures.insert(
            key.clone(),
            LiveTexture {
                source_id: source_id.clone(),
                texture: texture.downgrade(),
                bytes,
                class,
            },
        );
        let last_used = self.next_access();
        self.bytes = self.bytes.saturating_add(bytes);
        if class == TextureClass::Thumbnail {
            self.thumbnail_bytes = self.thumbnail_bytes.saturating_add(bytes);
        }
        self.entries.insert(
            key.clone(),
            TextureEntry {
                texture,
                bytes,
                last_used,
                class,
            },
        );
        self.order_mut(class)
            .insert(TextureAccess { last_used, key });
        self.evict_class_to_limits(class);
        let max_live_textures = self
            .max_thumbnail_textures
            .saturating_add(self.max_large_textures);
        if self.live_textures.len() > self.entries.len().saturating_add(max_live_textures) {
            self.live_textures
                .retain(|_, entry| entry.texture.upgrade().is_some());
        }
    }

    fn get_or_revive(&mut self, key: &K) -> Option<gdk::Texture> {
        if let Some(texture) = self.get(key) {
            return Some(texture);
        }
        let live = self.live_textures.get(key)?.clone();
        let Some(texture) = live.texture.upgrade() else {
            self.live_textures.remove(key);
            return None;
        };
        self.insert_with_class(
            key.clone(),
            live.source_id,
            texture.clone(),
            live.bytes,
            live.class,
        );
        Some(texture)
    }

    fn invalidate_source(&mut self, source_id: &SourceId) {
        let stale = self
            .live_textures
            .iter()
            .filter(|(_, entry)| &entry.source_id == source_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in stale {
            self.remove(&key);
            self.live_textures.remove(&key);
        }
    }

    fn remove(&mut self, key: &K) -> Option<TextureEntry> {
        let entry = self.entries.remove(key)?;
        self.order_mut(entry.class).remove(&TextureAccess {
            last_used: entry.last_used,
            key: key.clone(),
        });
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        if entry.class == TextureClass::Thumbnail {
            self.thumbnail_bytes = self.thumbnail_bytes.saturating_sub(entry.bytes);
        }
        Some(entry)
    }

    fn next_access(&mut self) -> u64 {
        self.next_access = self.next_access.wrapping_add(1).max(1);
        self.next_access
    }

    fn order_mut(&mut self, class: TextureClass) -> &mut BTreeSet<TextureAccess<K>> {
        match class {
            TextureClass::Thumbnail => &mut self.thumbnail_order,
            TextureClass::Large => &mut self.large_order,
        }
    }

    fn oldest_key(&self, class: TextureClass) -> Option<K> {
        match class {
            TextureClass::Thumbnail => self.thumbnail_order.first(),
            TextureClass::Large => self.large_order.first(),
        }
        .map(|access| access.key.clone())
    }

    fn class_usage(&self, class: TextureClass) -> (usize, usize) {
        match class {
            TextureClass::Thumbnail => (self.thumbnail_order.len(), self.thumbnail_bytes),
            TextureClass::Large => (
                self.large_order.len(),
                self.bytes.saturating_sub(self.thumbnail_bytes),
            ),
        }
    }

    fn class_limits(&self, class: TextureClass) -> (usize, usize) {
        match class {
            TextureClass::Thumbnail => (self.max_thumbnail_textures, self.max_thumbnail_bytes),
            TextureClass::Large => (self.max_large_textures, self.max_large_bytes),
        }
    }

    fn evict_class_to_limits(&mut self, class: TextureClass) {
        let limits = self.class_limits(class);
        while {
            let usage = self.class_usage(class);
            usage.0 > limits.0 || usage.1 > limits.1
        } {
            let Some(key) = self.oldest_key(class) else {
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
        let thumbnail_bytes = self
            .entries
            .values()
            .filter(|entry| entry.class == TextureClass::Thumbnail)
            .map(|entry| entry.bytes)
            .sum::<usize>();
        assert_eq!(self.bytes, bytes);
        assert_eq!(self.thumbnail_bytes, thumbnail_bytes);
        assert!(self.thumbnail_bytes <= self.max_thumbnail_bytes);
        assert!(self.bytes.saturating_sub(self.thumbnail_bytes) <= self.max_large_bytes);
        assert!(self.thumbnail_order.len() <= self.max_thumbnail_textures);
        assert!(self.large_order.len() <= self.max_large_textures);
        assert_eq!(
            self.thumbnail_order.len(),
            self.entries
                .values()
                .filter(|entry| entry.class == TextureClass::Thumbnail)
                .count()
        );
        assert_eq!(
            self.large_order.len(),
            self.entries
                .values()
                .filter(|entry| entry.class == TextureClass::Large)
                .count()
        );
        for access in &self.thumbnail_order {
            assert_eq!(
                self.entries.get(&access.key).map(|entry| entry.class),
                Some(TextureClass::Thumbnail)
            );
        }
        for access in &self.large_order {
            assert_eq!(
                self.entries.get(&access.key).map(|entry| entry.class),
                Some(TextureClass::Large)
            );
        }
        for (key, entry) in &self.entries {
            let access = TextureAccess {
                last_used: entry.last_used,
                key: key.clone(),
            };
            assert!(match entry.class {
                TextureClass::Thumbnail => self.thumbnail_order.contains(&access),
                TextureClass::Large => self.large_order.contains(&access),
            });
        }
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::with_class_limits(
            MAX_THUMBNAIL_TEXTURES,
            MAX_THUMBNAIL_TEXTURE_BYTES,
            MAX_RECENT_LARGE_TEXTURES,
            MAX_RECENT_LARGE_TEXTURE_BYTES,
        )
    }
}

impl TextureCache {
    pub(super) fn texture(
        &mut self,
        source_id: &SourceId,
        image: Arc<DecodedImage>,
    ) -> Option<gdk::Texture> {
        let identity = image.identity();
        if let Some(texture) = self.get_or_revive(&identity) {
            return Some(texture);
        }
        let bytes = image.rgba().len();
        let class = texture_class(image.width(), image.height());
        let texture = texture_from_decoded(image)?;
        self.insert_with_class(identity, source_id.clone(), texture.clone(), bytes, class);
        Some(texture)
    }

    pub(super) fn release_source(&mut self, source_id: &SourceId) {
        self.invalidate_source(source_id);
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

    #[test]
    fn texture_class_uses_the_longest_decoded_edge() {
        assert_eq!(texture_class(96, 96), TextureClass::Thumbnail);
        assert_eq!(texture_class(192, 48), TextureClass::Large);
    }

    #[test]
    fn recent_large_cache_reuses_one_texture_and_evicts_by_owned_bytes() {
        let source = SourceId::new("texture-cache-source");
        let mut cache = TextureCache::<u8>::with_limits(3, 8);
        let first = texture(1);
        let second = texture(2);
        cache.insert_with_class(1, source.clone(), first.clone(), 4, TextureClass::Large);
        cache.insert_with_class(2, source.clone(), second.clone(), 4, TextureClass::Large);
        let reused = cache.get(&1).expect("the first texture remains cached");
        assert_eq!(reused.as_ptr(), first.as_ptr());

        cache.insert_with_class(3, source, texture(3), 4, TextureClass::Large);

        assert_eq!(cache.bytes, 8);
        assert!(cache.get(&1).is_some(), "the recent texture stays cached");
        assert!(cache.get(&2).is_none(), "the older strong entry is evicted");
        let revived = cache
            .get_or_revive(&2)
            .expect("a mounted texture remains interned after LRU eviction");
        assert_eq!(revived.as_ptr(), second.as_ptr());
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn recent_large_textures_do_not_displace_source_thumbnails() {
        let source = SourceId::new("independent-texture-budget-source");
        let mut cache = TextureCache::<u8>::with_class_limits(2, 8, 2, 8);
        cache.insert_with_class(1, source.clone(), texture(1), 4, TextureClass::Thumbnail);
        cache.insert_with_class(2, source.clone(), texture(2), 4, TextureClass::Thumbnail);
        for key in 3..=5 {
            cache.insert_with_class(key, source.clone(), texture(key), 4, TextureClass::Large);
        }

        assert_eq!(cache.bytes, 16);
        assert_eq!(cache.thumbnail_bytes, 8);
        assert!(cache.get(&1).is_some());
        assert!(cache.get(&2).is_some());
        assert!(cache.get(&3).is_none());
        assert!(cache.get(&4).is_some());
        assert!(cache.get(&5).is_some());
    }

    #[test]
    fn large_textures_keep_only_their_recent_window() {
        let source = SourceId::new("recent-large-texture-source");
        let mut cache = TextureCache::<u8>::with_class_limits(8, 32, 8, 8);

        for key in 1..=4 {
            cache.insert_with_class(key, source.clone(), texture(key), 4, TextureClass::Large);
        }

        assert_eq!(cache.bytes.saturating_sub(cache.thumbnail_bytes), 8);
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&3).is_some());
        assert!(cache.get(&4).is_some());
    }

    #[test]
    fn normal_grid_covers_use_the_recent_large_byte_window() {
        let source = SourceId::new("normal-grid-texture-source");
        let cover_bytes = 200 * 200 * 4;
        let expected = MAX_RECENT_LARGE_TEXTURE_BYTES / cover_bytes;
        let mut cache = TextureCache::<u16>::with_class_limits(
            MAX_THUMBNAIL_TEXTURES,
            MAX_THUMBNAIL_TEXTURE_BYTES,
            MAX_RECENT_LARGE_TEXTURES,
            MAX_RECENT_LARGE_TEXTURE_BYTES,
        );

        for key in 0..=u16::try_from(expected).expect("grid-cover window fits in u16") {
            cache.insert_with_class(
                key,
                source.clone(),
                texture(key as u8),
                cover_bytes,
                TextureClass::Large,
            );
        }

        assert_eq!(cache.large_order.len(), expected);
        assert!(cache.get(&0).is_none());
        assert!(cache.bytes <= MAX_RECENT_LARGE_TEXTURE_BYTES);
    }

    #[test]
    fn source_release_drops_only_that_sources_textures() {
        let first_source = SourceId::new("first-texture-source");
        let second_source = SourceId::new("second-texture-source");
        let mut cache = TextureCache::<u8>::with_limits(3, 12);
        cache.insert_with_class(1, first_source.clone(), texture(1), 4, TextureClass::Large);
        cache.insert_with_class(2, second_source, texture(2), 4, TextureClass::Large);

        cache.invalidate_source(&first_source);

        assert!(cache.get_or_revive(&1).is_none());
        assert!(cache.get_or_revive(&2).is_some());
        assert_eq!(cache.bytes, 4);
    }

    proptest! {
        #[test]
        fn arbitrary_cache_operations_preserve_each_class_bound(
            operations in prop::collection::vec(
                (0u8..6, 0u8..24, 1usize..=24, any::<bool>(), any::<bool>()),
                1..=96,
            ),
        ) {
            let first_source = SourceId::new("property-source-one");
            let second_source = SourceId::new("property-source-two");
            let mut cache = TextureCache::<u8>::with_class_limits(16, 64, 8, 32);

            for (operation, key, bytes, thumbnail, second) in operations {
                let source = if second {
                    second_source.clone()
                } else {
                    first_source.clone()
                };
                match operation {
                    0 | 1 => cache.insert_with_class(
                        key,
                        source,
                        texture(key),
                        bytes,
                        if thumbnail {
                            TextureClass::Thumbnail
                        } else {
                            TextureClass::Large
                        },
                    ),
                    2 => {
                        cache.get(&key);
                    }
                    3 => {
                        cache.remove(&key);
                    }
                    4 => cache.invalidate_source(&source),
                    _ => {
                        cache.get_or_revive(&key);
                    }
                }
                cache.assert_consistent();
            }
        }
    }
}
