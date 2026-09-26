//! Decodes stored artwork bindings into images in selection order.

use sources::{ExternalAlbumImageRef, LocalImageRef, NativeArtworkBinding};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Candidate {
    Native(NativeArtworkBinding),
    Local(LocalImageRef),
    Playlist(library::PlaylistArtworkBinding),
    Album(metadata_lookup::AlbumCover),
    Artist {
        name: String,
        musicbrainz_id: Option<String>,
    },
}

impl Candidate {
    pub(crate) fn asset_identity(&self) -> String {
        match self {
            Self::Native(image) => {
                format!("native\0{}\0{}", image.source_id, image.image.item_id)
            }
            Self::Local(LocalImageRef::File {
                source_id, path, ..
            }) => {
                format!("local-file\0{source_id}\0{path}")
            }
            Self::Local(LocalImageRef::Embedded {
                source_id,
                path,
                picture_index,
                ..
            }) => format!("local-embedded\0{source_id}\0{path}\0{picture_index}"),
            Self::Album(album) => album.stable_identity(),
            Self::Playlist(image) => format!(
                "{}\0{}\0{}",
                if image.smart {
                    "smart-playlist"
                } else {
                    "playlist"
                },
                image.source_id.as_deref().unwrap_or_default(),
                image.object_id
            ),
            Self::Artist {
                name,
                musicbrainz_id,
                ..
            } => {
                format!(
                    "artist\0{name}\0{}",
                    musicbrainz_id.as_deref().unwrap_or_default()
                )
            }
        }
    }

    pub(crate) fn stable_identity(&self) -> String {
        let mut identity = match self {
            Self::Native(image) => format!(
                "native\0{}\0{}\0{}",
                image.source_id,
                image.image.item_id,
                image.image.tag.as_deref().unwrap_or_default()
            ),
            Self::Local(LocalImageRef::File { path, revision, .. }) => {
                format!("local-file\0{path}\0{revision}")
            }
            Self::Local(LocalImageRef::Embedded {
                path,
                picture_index,
                revision,
                ..
            }) => format!("local-embedded\0{path}\0{picture_index}\0{revision}"),
            Self::Album(album) => album.stable_identity(),
            Self::Playlist(image) => format!("{}\0{}", self.asset_identity(), image.revision),
            Self::Artist { .. } => format!("{}\0\0", self.asset_identity()),
        };
        if let Self::Local(binding) = self
            && binding.source_id().as_str() != sources::LOCAL_LIBRARY_SOURCE_ID
        {
            identity.push('\0');
            identity.push_str(binding.source_id().as_str());
        }
        identity
    }

    pub(crate) const fn is_external(&self) -> bool {
        matches!(self, Self::Album(_) | Self::Artist { .. })
    }

    pub(crate) fn source_id(&self) -> Option<&sources::SourceId> {
        match self {
            Self::Native(image) => Some(&image.source_id),
            Self::Local(image) => Some(image.source_id()),
            Self::Album(_) | Self::Artist { .. } | Self::Playlist(_) => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkBinding {
    pub(crate) candidates: Vec<Candidate>,
    asset_identity: String,
    stable_identity: String,
}

impl ArtworkBinding {
    pub fn opaque(binding: &[u8]) -> Self {
        if let Some(candidate) = Self::leaf(binding) {
            return Self {
                asset_identity: candidate.asset_identity(),
                stable_identity: candidate.stable_identity(),
                candidates: vec![candidate],
            };
        }
        let Ok(artist) = serde_json::from_slice::<library::ArtistArtworkBinding>(binding) else {
            return Self::default();
        };
        let source = artist.source.as_deref().and_then(Self::leaf);
        let fallback = artist.fallback.as_deref().and_then(Self::leaf);
        let portrait = Candidate::Artist {
            name: artist.artist_name,
            musicbrainz_id: artist.musicbrainz_artist_id,
        };
        let asset_identity = portrait.asset_identity();
        let stable_identity = format!(
            "{asset_identity}\0{}\0{}",
            source
                .as_ref()
                .map(Candidate::stable_identity)
                .unwrap_or_default(),
            fallback
                .as_ref()
                .map(Candidate::stable_identity)
                .unwrap_or_default(),
        );
        Self {
            candidates: source
                .into_iter()
                .chain(Some(portrait))
                .chain(fallback)
                .collect(),
            asset_identity,
            stable_identity,
        }
    }

    fn leaf(binding: &[u8]) -> Option<Candidate> {
        serde_json::from_slice::<library::PlaylistArtworkBinding>(binding)
            .ok()
            .map(Candidate::Playlist)
            .or_else(|| {
                serde_json::from_slice::<NativeArtworkBinding>(binding)
                    .ok()
                    .map(Candidate::Native)
            })
            .or_else(|| {
                serde_json::from_slice::<LocalImageRef>(binding)
                    .ok()
                    .map(Candidate::Local)
            })
            .or_else(|| {
                let external = serde_json::from_slice::<ExternalAlbumImageRef>(binding).ok()?;
                metadata_lookup::AlbumCover::new(
                    &external.artist,
                    &external.album,
                    external.musicbrainz_release_group_id.as_deref(),
                    external.musicbrainz_release_id.as_deref(),
                )
                .map(Candidate::Album)
            })
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn has_external(&self) -> bool {
        self.candidates.iter().any(Candidate::is_external)
    }

    pub(crate) fn source_id(&self) -> Option<&sources::SourceId> {
        self.candidates.iter().find_map(Candidate::source_id)
    }

    pub(crate) fn asset_identity(&self) -> &str {
        &self.asset_identity
    }

    pub fn stable_identity(&self) -> &str {
        &self.stable_identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_binding_decodes_exactly_one_selected_source_request() {
        let encoded = sources::native_artwork_binding(
            "source",
            &sources::NativeImageRef::new("album", Some("tag".to_string())),
        )
        .expect("encode binding");
        let binding = ArtworkBinding::opaque(&encoded);
        assert!(!binding.candidates.is_empty());
        assert!(binding.stable_identity().contains("album"));
    }

    #[test]
    fn released_local_bindings_keep_their_cache_identity() {
        for (encoded, identity) in [
            (
                br#"{"File":{"path":"/music/cover.jpg","revision":"one"}}"#.as_slice(),
                "local-file\0/music/cover.jpg\0one",
            ),
            (
                br#"{"Embedded":{"path":"/music/track.flac","picture_index":2,"revision":"two"}}"#
                    .as_slice(),
                "local-embedded\0/music/track.flac\x002\0two",
            ),
        ] {
            let binding = ArtworkBinding::opaque(encoded);
            assert_eq!(binding.stable_identity(), identity);
            let Some(Candidate::Local(reference)) = binding.candidates.first() else {
                panic!("released Local binding");
            };
            assert_eq!(
                reference.source_id().as_str(),
                sources::LOCAL_LIBRARY_SOURCE_ID
            );
        }
    }
}
