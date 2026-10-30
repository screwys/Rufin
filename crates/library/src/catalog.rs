//! Canonical observations and concrete source membership writes.
use crate::{Catalog, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey};
use sqlx::{FromRow, SqliteConnection};
use std::collections::{HashMap, HashSet};
pub(crate) mod albums;
pub(crate) mod artwork;
pub(crate) mod collections;
pub(crate) mod families;
pub(crate) mod fetch;
pub(crate) mod file_reads;
pub(crate) mod files;
pub(crate) mod image;
pub(crate) mod load;
pub(crate) mod merge;
mod observations;
pub(crate) mod records;
mod rows;
pub(crate) mod snapshot;
pub(crate) mod strings;
pub(crate) mod views;
pub(crate) mod write;
pub use observations::*;
pub use write::{Change, Scope, SourcePhase, StateChange};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Freshness(Vec<u8>);

impl Freshness {
    pub fn new(marker: impl Into<Vec<u8>>) -> LibraryResult<Self> {
        let marker = marker.into();
        if marker.len() > 64 * 1024 {
            return Err(LibraryError::InvalidRequest(
                "freshness marker exceeds 64 KiB".into(),
            ));
        }
        Ok(Self(marker))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub struct Observation<'a> {
    pub catalog: &'a Catalog,
    pub source_id: &'a str,
    /// The stamp file rows carry for this observation.
    pub observed: Option<i64>,
    pub source: SourceKey,
    /// The walk `start_observation` opened: a listing whose completion hides what it did not
    /// see. Without one, every Write of this observation is a live change.
    pub(crate) walk: Option<u64>,
    /// Some committed Write of this observation changed what the catalog lists show.
    pub(crate) changed: bool,
    /// Folder members this walk listed, by folder.
    pub(crate) memberships: HashMap<i64, HashSet<i64>>,
    /// The entries of a source playlist read so far, until its last page.
    pub(crate) playlist_entries: Vec<(String, crate::TrackKey)>,
}

/// The table and key column of a source-owned record kind.
pub(crate) fn table(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "track" => ("tracks", "track_key"),
        "album" => ("albums", "album_key"),
        "artist" => ("artists", "artist_key"),
        "folder" => ("folders", "folder_key"),
        _ => ("tags", "tag_key"),
    }
}

impl<'a> Observation<'a> {
    /// A Write of this observation; inside a walk it carries the walk's seen set.
    pub(crate) async fn write(&self) -> LibraryResult<write::Write<'a>> {
        let mut write = self.catalog.write().await?;
        write.walk = self.walk;
        Ok(write)
    }

    /// Whether this observation is a listing that `start_observation` opened.
    pub fn is_walk(&self) -> bool {
        self.walk.is_some()
    }

    /// The open walk. Completion needs one, so it hides only what a listing missed.
    pub(crate) fn walk(&self) -> LibraryResult<u64> {
        self.walk
            .ok_or_else(|| LibraryError::InvalidRequest("the observation was not started".into()))
    }
}

impl Drop for Observation<'_> {
    fn drop(&mut self) {
        if let Some(walk) = self.walk {
            self.catalog.close_walk(walk);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedSource {
    pub source: SourceKey,
    pub object_id: String,
    pub display_name: String,
    pub catalog_complete: bool,
}

#[derive(Clone, Debug, FromRow)]
pub struct LocalArtworkCandidate {
    pub object_id: String,
    pub path: String,
    pub root: Option<String>,
    pub picture_index: Option<i64>,
    pub artist_pictures: Option<String>,
    pub primary_artist: bool,
    pub revision: Option<String>,
    pub disc_number: i64,
    pub track_number: i64,
    pub sort_text: String,
}

impl Catalog {
    pub async fn observe<'a>(
        &'a self,
        source_id: &'a str,
        observed: Option<i64>,
    ) -> LibraryResult<Observation<'a>> {
        self.loaded().await;
        let source = self
            .source_key(&SourceId::new(source_id))
            .ok_or_else(|| LibraryError::InvalidRequest("source is no longer configured".into()))?;
        Ok(Observation {
            catalog: self,
            source_id,
            source,
            observed,
            walk: None,
            changed: false,
            memberships: HashMap::new(),
            playlist_entries: Vec::new(),
        })
    }

    /// A source the Catalog holds. Callers that can run before the first load await
    /// `loaded` first.
    pub fn cached_source(&self, source_id: &str) -> Option<CachedSource> {
        self.snapshot()
            .sources
            .values()
            .find(|source| source.object == source_id)
            .map(|source| CachedSource {
                source: source.key,
                object_id: source.object.clone(),
                display_name: source.name.clone(),
                catalog_complete: source.complete,
            })
    }

    pub async fn accept_freshness(
        &self,
        source_id: &str,
        freshness: &Freshness,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sources WHERE object_id=?1 AND catalog_complete AND freshness=?2)")
            .bind(source_id).bind(freshness.as_bytes()).fetch_one(&mut *connection).await?)
    }
}
