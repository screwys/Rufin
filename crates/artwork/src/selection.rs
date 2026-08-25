//! Decodes one already-selected opaque Library artwork binding.

use std::sync::Arc;

use sources::{ExternalAlbumImageRef, LocalImageRef, NativeImageRef};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Candidate {
    Native(NativeImageRef),
    Local(LocalImageRef),
    Album(metadata_lookup::AlbumCover),
}

impl Candidate {
    pub(crate) fn stable_identity(&self) -> String {
        match self {
            Self::Native(image) => format!(
                "native\0{}\0{}",
                image.item_id,
                image.tag.as_deref().unwrap_or_default()
            ),
            Self::Local(LocalImageRef::File { path, revision }) => {
                format!("local-file\0{path}\0{revision}")
            }
            Self::Local(LocalImageRef::Embedded {
                path,
                picture_index,
                revision,
            }) => format!("local-embedded\0{path}\0{picture_index}\0{revision}"),
            Self::Album(album) => album.stable_identity(),
        }
    }

    pub(crate) const fn is_external(&self) -> bool {
        matches!(self, Self::Album(_))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkBinding {
    candidates: Arc<[Candidate]>,
    stable_identity: Arc<str>,
}

impl ArtworkBinding {
    pub fn opaque(binding: &[u8]) -> Self {
        let candidate = serde_json::from_slice::<NativeImageRef>(binding)
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
        let stable_identity: Arc<str> = candidate
            .as_ref()
            .map(Candidate::stable_identity)
            .unwrap_or_default()
            .into();
        Self {
            candidates: candidate.into_iter().collect(),
            stable_identity,
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub(crate) fn has_external(&self) -> bool {
        self.candidates.iter().any(Candidate::is_external)
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
        let encoded = serde_json::to_vec(&NativeImageRef::new("album", Some("tag".to_string())))
            .expect("encode binding");
        let binding = ArtworkBinding::opaque(&encoded);
        assert_eq!(binding.candidates().len(), 1);
        assert!(binding.stable_identity().contains("album"));
    }
}
