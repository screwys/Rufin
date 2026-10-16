//! Decodes stored artwork bindings into images in selection order.

pub(crate) use library::ArtworkCandidate as Candidate;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkBinding {
    pub(crate) candidates: Vec<Candidate>,
    asset_identity: String,
    stable_identity: String,
}

impl ArtworkBinding {
    pub fn opaque(binding: &[u8]) -> Self {
        let mut candidates: Vec<Candidate> = serde_json::from_slice(binding).unwrap_or_default();
        for candidate in &mut candidates {
            if candidate.is_automatic_playlist() {
                *candidate = Candidate::new(candidate.image.clone(), candidate.blur_hash.clone());
            }
        }
        let asset_identity = candidates
            .iter()
            .find(|candidate| matches!(candidate.image, library::ArtworkImage::Artist { .. }))
            .or_else(|| candidates.first())
            .map(Candidate::asset_identity)
            .unwrap_or_default();
        let stable_identity = candidates
            .iter()
            .map(Candidate::stable_identity)
            .collect::<Vec<_>>()
            .join("\0");
        Self {
            candidates,
            asset_identity,
            stable_identity,
        }
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
        let album = library::AlbumCover::new(
            " Artist ",
            " Album ",
            Some(" release-group "),
            Some("bad/id"),
        )
        .unwrap();
        let candidate = Candidate::new(library::ArtworkImage::Album(album), None);
        assert_eq!(
            candidate.stable_identity(),
            "album\0Artist\0Album\0release-group\0"
        );
        assert_eq!(
            serde_json::to_value(&candidate.image).unwrap(),
            serde_json::json!({
                "kind":"album", "artist":"Artist", "album":"Album", "release_group_id":"release-group", "release_id":null
            })
        );
        let restored: Candidate =
            serde_json::from_slice(&serde_json::to_vec(&candidate).unwrap()).unwrap();
        assert_eq!(restored, candidate);
        assert!(library::AlbumCover::new("Unknown Artist", "Unknown Album", None, None).is_none());
        for encoded in [
            encoded,
            br#"{"File":{"path":"/cover.png","revision":"one"}}"#.to_vec(),
            br#"{"Embedded":{"path":"/song.flac","picture_index":0,"revision":"one"}}"#.to_vec(),
            br#"{"artist":"Artist","album":"Album","musicbrainz_release_group_id":null,"musicbrainz_release_id":null}"#.to_vec(),
            br#"{"kind":"artist","artist_name":"Artist","musicbrainz_artist_id":null,"source":null,"fallback":null}"#.to_vec(),
            br#"{"kind":"playlist","smart":false,"source_id":null,"object_id":"playlist","revision":"one"}"#.to_vec(),
        ] {
            let normalized = library::normalize_artwork_binding(&encoded).unwrap();
            let before = ArtworkBinding::opaque(&normalized);
            let mut candidates = before.candidates.clone();
            for candidate in &mut candidates { candidate.blur_hash = Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into()); }
            let after = ArtworkBinding::opaque(&serde_json::to_vec(&candidates).unwrap());
            assert_eq!(after.stable_identity(), before.stable_identity());
            assert_eq!(after.asset_identity(), before.asset_identity());
            assert!(after.candidates.iter().all(|candidate| candidate.blur_hash().is_some()));
        }
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
            let binding =
                ArtworkBinding::opaque(&library::normalize_artwork_binding(encoded).unwrap());
            assert_eq!(binding.stable_identity(), identity);
            let reference = binding.candidates.first().unwrap();
            assert_eq!(
                reference.source_id().unwrap().as_str(),
                sources::LOCAL_LIBRARY_SOURCE_ID
            );
        }
    }
}
