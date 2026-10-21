//! Canonical observations and concrete source membership writes.
use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey};
use sqlx::{Connection, FromRow, SqliteConnection};
use std::sync::Arc;
pub(crate) mod artwork;
pub(crate) mod cache;
pub(crate) mod collections;
pub(crate) mod file_reads;
pub(crate) mod files;
pub(crate) mod index;
mod observations;
pub(crate) use cache::{remember_collection, remember_track};
pub use observations::*;

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogUpdate {
    pub metadata: bool,
    pub activity: bool,
    pub playlists: bool,
    pub artwork: bool,
    pub home: bool,
    pub track_rekeys: Arc<[(crate::TrackKey, crate::TrackKey)]>,
}

impl CatalogUpdate {
    pub fn changed(&self) -> bool {
        self.metadata || self.activity || self.playlists || self.artwork || self.home
    }

    pub fn merge(&mut self, other: Self) {
        self.metadata |= other.metadata;
        self.activity |= other.activity;
        self.playlists |= other.playlists;
        self.artwork |= other.artwork;
        self.home |= other.home;
        if !other.track_rekeys.is_empty() {
            let mut pairs = self.track_rekeys.to_vec();
            pairs.extend_from_slice(&other.track_rekeys);
            self.track_rekeys = pairs.into();
        }
    }
}

pub struct Catalog<'a> {
    pub database: &'a Database,
    pub source_id: &'a str,
    pub observed: Option<i64>,
    pub source: SourceKey,
    pub(crate) changes: CatalogUpdate,
    pub(crate) incoming: bool,
}

pub(crate) fn keep_local_membership(kind: &str, incoming: bool) -> String {
    if !incoming {
        return String::new();
    }
    let member = match kind {
            "track" => "EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=tracks.track_key AND access.local_file_key IS NOT NULL)".to_owned(),
            "album" => "EXISTS(SELECT 1 FROM tracks member WHERE member.album_key=albums.album_key AND member.indexed=1 AND EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=member.track_key AND access.local_file_key IS NOT NULL))".to_owned(),
            "artist" => "EXISTS(SELECT 1 FROM tracks member JOIN track_artists edge USING(track_key) WHERE edge.artist_key=artists.artist_key AND member.indexed=1 AND EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=member.track_key AND access.local_file_key IS NOT NULL)) OR EXISTS(SELECT 1 FROM tracks member JOIN track_album_artists edge USING(track_key) WHERE edge.artist_key=artists.artist_key AND member.indexed=1 AND EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=member.track_key AND access.local_file_key IS NOT NULL))".to_owned(),
            "genre" | "mood" | "folder" => format!("EXISTS(SELECT 1 FROM tracks member JOIN track_{kind}s edge USING(track_key) WHERE edge.{kind}_key={kind}s.{kind}_key AND member.indexed=1 AND EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=member.track_key AND access.local_file_key IS NOT NULL))"),
            _ => return String::new(),
        };
    format!(" AND NOT((SELECT kind FROM sources WHERE source_key=?1)='local' AND({member}))")
}

impl Catalog<'_> {
    pub(crate) async fn begin_write<'c>(
        &self,
        connection: &'c mut SqliteConnection,
    ) -> LibraryResult<sqlx::Transaction<'c, sqlx::Sqlite>> {
        let mut transaction = connection.begin().await?;
        if self.incoming {
            crate::connect::set_receiving_on(&mut transaction, true).await?;
        }
        Ok(transaction)
    }

    pub(crate) async fn commit_write(
        &self,
        mut transaction: sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> LibraryResult<()> {
        if self.incoming {
            crate::connect::set_receiving_on(&mut transaction, false).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub fn changes(&self) -> &CatalogUpdate {
        &self.changes
    }

    pub fn into_changes(self) -> CatalogUpdate {
        self.changes
    }
}

#[derive(Clone, Debug, FromRow, Eq, PartialEq)]
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

impl Database {
    pub async fn catalog<'a>(
        &'a self,
        source_id: &'a str,
        observed: Option<i64>,
    ) -> LibraryResult<Catalog<'a>> {
        let source = self
            .source_identity_key(&SourceId::new(source_id))
            .await?
            .ok_or_else(|| LibraryError::InvalidRequest("source is no longer configured".into()))?;
        Ok(Catalog {
            database: self,
            source_id,
            source,
            observed,
            changes: CatalogUpdate::default(),
            incoming: false,
        })
    }

    pub async fn publish_catalog_update(&self, source_id: &str, update: CatalogUpdate) {
        if !update.changed() && update.track_rekeys.is_empty() {
            return;
        }
        let source_id = SourceId::new(source_id);
        match self.source_identity_key(&source_id).await {
            Ok(Some(source)) => {
                self.catalog_updated(Some((source_id, source)), update)
                    .await
            }
            Ok(None) => {
                tracing::warn!(%source_id,"could not publish catalog update for removed source")
            }
            Err(error) => tracing::warn!(%error,"could not read source for catalog update"),
        }
    }

    pub async fn reconcile_source(
        &self,
        source_id: &SourceId,
        kind: &str,
        name: &str,
    ) -> LibraryResult<CachedSource> {
        self.reconcile_source_cache(source_id, kind, name, false)
            .await
    }

    pub(crate) async fn reconcile_source_cache(
        &self,
        source_id: &SourceId,
        kind: &str,
        name: &str,
        incoming: bool,
    ) -> LibraryResult<CachedSource> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if incoming {
            crate::connect::set_receiving_on(&mut transaction, true).await?;
        }
        let source = crate::db::write_source_identity(&mut transaction, source_id.as_str()).await?;
        let changed = sqlx::query("UPDATE sources SET kind=?2,display_name=?3,normalized_name=?4,distinct_track_covers=?5 WHERE source_key=?1 AND (kind,display_name,normalized_name,distinct_track_covers) IS NOT (?2,?3,?4,?5)")
            .bind(source).bind(kind).bind(name).bind(name.to_lowercase()).bind(self.distinct_track_covers()).execute(&mut *transaction).await?.rows_affected()>0;
        if changed {
            crate::connect::mark_current_on(
                &mut transaction,
                "catalog:source",
                &serde_json::json!([source_id.as_str(), source_id.as_str()]).to_string(),
                false,
            )
            .await?;
        }
        let row = sqlx::query_as("SELECT source_key source,object_id,display_name,catalog_complete FROM sources WHERE source_key=?1")
            .bind(source).fetch_one(&mut *transaction).await?;
        if incoming {
            crate::connect::set_receiving_on(&mut transaction, false).await?;
        }
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn cached_source(
        &self,
        source_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CachedSource>> {
        let mut connection = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
            connection = self.acquire_reader() => connection?,
        };
        Ok(sqlx::query_as("SELECT source_key source,object_id,display_name,catalog_complete FROM sources WHERE object_id=?1")
            .bind(source_id).fetch_optional(&mut *connection).await?)
    }

    pub async fn accept_freshness(
        &self,
        source_id: &str,
        freshness: &Freshness,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<bool> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sources WHERE object_id=?1 AND catalog_complete AND freshness=?2)")
            .bind(source_id).bind(freshness.as_bytes()).fetch_one(&mut *connection).await?)
    }

    pub async fn invalidate_source_observation(&self, source_id: &SourceId) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("UPDATE sources SET observation_stamp=observation_stamp+1,freshness=NULL WHERE object_id=?1")
            .bind(source_id.as_str()).execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
        Ok(())
    }
}
