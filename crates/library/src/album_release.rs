//! Exact Album release lookup state.
//!
//! Source Album facts remain canonical. A matching found result is overlaid on
//! hydration or patched into the selected Library; missing results only
//! prevent repeated lookup of the same exact identity.

use crate::{AcceptedLibraryChange, AlbumId, Library, LibraryError, LibraryResult, SourceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlbumReleaseIdentity {
    ReleaseGroup(String),
    Release(String),
}

impl AlbumReleaseIdentity {
    pub(crate) fn stored_key(&self) -> String {
        match self {
            Self::ReleaseGroup(id) => format!("release-group:{id}"),
            Self::Release(id) => format!("release:{id}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumReleaseCandidate {
    pub source_id: SourceId,
    pub album_id: AlbumId,
    pub identity: AlbumReleaseIdentity,
    pub(crate) library_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlbumReleaseResult {
    Found { release_types: Vec<String> },
    Missing,
}

impl Library {
    pub fn take_album_release_lookups(
        &self,
        limit: usize,
    ) -> LibraryResult<Vec<AlbumReleaseCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self.store.album_release_candidates(
            self.source_id().clone(),
            self.library_id(),
            limit,
        )?)
    }

    pub fn accept_album_release_result(
        &self,
        candidate: AlbumReleaseCandidate,
        result: AlbumReleaseResult,
    ) -> LibraryResult<Option<AcceptedLibraryChange>> {
        if self.source_id() != &candidate.source_id || self.library_id() != candidate.library_id {
            return Ok(None);
        }
        if matches!(
            &result,
            AlbumReleaseResult::Found { release_types, .. }
                if release_types.is_empty()
                    || release_types.iter().any(|value| value.trim().is_empty())
        ) {
            return Err(LibraryError::Persistence(
                "found Album release result cannot be empty".to_string(),
            ));
        }
        let accepted = self
            .store
            .accept_album_release(candidate.clone(), result.clone())?;
        if !accepted {
            return Ok(None);
        }
        let AlbumReleaseResult::Found { release_types } = result else {
            return Ok(None);
        };
        Ok(Some(self.replace_album_release(
            &candidate.album_id,
            release_types,
        )?))
    }
}

pub(crate) fn release_identity(album: &crate::Album) -> Option<AlbumReleaseIdentity> {
    album
        .musicbrainz_release_group_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .cloned()
        .map(AlbumReleaseIdentity::ReleaseGroup)
        .or_else(|| {
            album
                .musicbrainz_album_id
                .as_ref()
                .filter(|id| !id.is_empty())
                .cloned()
                .map(AlbumReleaseIdentity::Release)
        })
}
