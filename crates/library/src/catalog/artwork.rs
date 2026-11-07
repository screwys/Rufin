//! The artist candidate in an artist's native artwork binding. Which picture a track, album or
//! artist shows is chosen when it is read, from the resident picture bits.

use crate::{ArtworkCandidate, ArtworkImage, LibraryResult};

pub(crate) fn artist_binding(
    name: &str,
    musicbrainz_id: Option<&str>,
    binding: Option<&[u8]>,
) -> LibraryResult<Vec<u8>> {
    let mut candidates = binding
        .map(serde_json::from_slice::<Vec<ArtworkCandidate>>)
        .transpose()?
        .unwrap_or_default();
    candidates.retain(|candidate| !matches!(candidate.image, ArtworkImage::Artist { .. }));
    candidates.push(ArtworkCandidate::new(
        ArtworkImage::Artist {
            name: name.to_owned(),
            musicbrainz_id: musicbrainz_id.map(str::to_owned),
        },
        None,
    ));
    Ok(serde_json::to_vec(&candidates)?)
}
