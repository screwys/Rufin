//! Decodes one already-selected opaque Library artwork binding.

use sources::{ExternalAlbumImageRef, LocalImageRef, NativeArtworkBinding};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Candidate {
    Native(NativeArtworkBinding),
    Local(LocalImageRef),
    Album(metadata_lookup::AlbumCover),
}

impl Candidate {
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
        matches!(self, Self::Album(_))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkBinding {
    candidate: Option<Candidate>,
    stable_identity: String,
}

impl ArtworkBinding {
    pub fn opaque(binding: &[u8]) -> Self {
        let candidate = serde_json::from_slice::<NativeArtworkBinding>(binding)
            .ok()
            .map(Candidate::Native)
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
            });
        let stable_identity = candidate
            .as_ref()
            .map(Candidate::stable_identity)
            .unwrap_or_default();
        Self {
            candidate,
            stable_identity,
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn candidate(&self) -> Option<&Candidate> {
        self.candidate.as_ref()
    }

    pub(crate) fn has_external(&self) -> bool {
        self.candidate.as_ref().is_some_and(Candidate::is_external)
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
        assert!(binding.candidate().is_some());
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
            let Some(Candidate::Local(reference)) = binding.candidate() else {
                panic!("released Local binding");
            };
            assert_eq!(
                reference.source_id().as_str(),
                sources::LOCAL_LIBRARY_SOURCE_ID
            );
        }
    }
}
