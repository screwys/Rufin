//! A source's tracks as another installation observes them, read by key.
use crate::{Catalog, Freshness, LibraryResult, SourceId, TrackObservation};

const SOURCE_KEY: &str = "(SELECT source_key FROM sources WHERE object_id=?1)";

#[derive(sqlx::FromRow)]
struct ObservedTrack {
    object_id: String,
    album_object_id: Option<String>,
    metadata: String,
    source_path: Option<String>,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
    first_seen_at: Option<i64>,
    audio_revision: Option<Vec<u8>>,
    audio_hash: Option<Vec<u8>>,
    source_lyrics: Option<String>,
}

impl Catalog {
    /// Changes when a track of the source is added, removed or its file is rewritten.
    pub async fn source_marker(&self, source: &SourceId) -> LibraryResult<Freshness> {
        let mut connection = self.acquire_reader().await?;
        let (tracks, modified, last): (i64, Option<i64>, Option<i64>) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT (SELECT count(*) FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1),
                    (SELECT max(mtime_ns) FROM files WHERE source_key={SOURCE_KEY}),
                    (SELECT max(track_key) FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1)"
            )))
            .bind(source.as_str())
            .fetch_one(&mut *connection)
            .await?;
        Freshness::new(
            [
                tracks,
                modified.unwrap_or_default(),
                last.unwrap_or_default(),
            ]
            .map(i64::to_be_bytes)
            .concat(),
        )
    }

    /// The object ids of the source's indexed tracks.
    pub async fn source_track_ids(&self, source: &SourceId) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT object_id FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1 AND object_id IS NOT NULL ORDER BY object_id"
        )))
        .bind(source.as_str())
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The source's indexed tracks whose file or CUE sheet changed after `since`, in unix
    /// seconds.
    pub async fn source_changed_since(
        &self,
        source: &SourceId,
        since: i64,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT track.object_id FROM tracks track JOIN track_details details USING(track_key)
             JOIN files file ON file.source_key=track.source_key AND file.path IN(details.source_path,details.cue_path)
             WHERE track.source_key={SOURCE_KEY} AND track.indexed=1 AND track.object_id IS NOT NULL AND file.mtime_ns>?2"
        )))
        .bind(source.as_str())
        .bind(since.saturating_mul(1_000_000_000))
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The source's indexed tracks of `ids` as observations: object ids, metadata,
    /// root-relative paths, the CUE window, audio keys and source lyrics. They name no media
    /// URI, picture or user state.
    pub async fn source_observations(
        &self,
        source: &SourceId,
        ids: &[String],
    ) -> LibraryResult<Vec<TrackObservation>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<ObservedTrack> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT track.object_id,album.object_id album_object_id,{} metadata,
                file.relative_path source_path,cue.relative_path cue_path,details.cue_start_millis,details.cue_end_millis,
                track.first_seen_at,details.audio_revision,details.audio_hash,
                (SELECT lyrics FROM lyrics_cache WHERE media_uri=track.media_uri AND authority='source' AND role='original' AND language='' AND script='') source_lyrics
             FROM json_each(?2) requested
             JOIN tracks track ON track.source_key={SOURCE_KEY} AND track.object_id=requested.value AND track.indexed=1
             LEFT JOIN track_details details USING(track_key)
             LEFT JOIN albums album ON album.album_key=track.album_key
             LEFT JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
             LEFT JOIN files cue ON cue.source_key=track.source_key AND cue.path=details.cue_path
             ORDER BY requested.key",
            crate::track_metadata_sql("track", "details")
        )))
        .bind(source.as_str())
        .bind(serde_json::to_string(ids)?)
        .fetch_all(&mut *connection)
        .await?;
        let hash = |bytes: Option<Vec<u8>>| bytes.and_then(|bytes| bytes.try_into().ok());
        rows.into_iter()
            .map(|row| {
                Ok(TrackObservation {
                    object_id: row.object_id,
                    album_object_id: row.album_object_id,
                    metadata: serde_json::from_str(&row.metadata)?,
                    source_path: row.source_path,
                    cue_path: row.cue_path,
                    cue_start_millis: row.cue_start_millis,
                    cue_end_millis: row.cue_end_millis,
                    first_seen_at: row.first_seen_at,
                    audio_revision: hash(row.audio_revision),
                    audio_hash: hash(row.audio_hash),
                    source_lyrics: row.source_lyrics,
                    ..Default::default()
                })
            })
            .collect()
    }

    /// The URI of a source's track by its object id.
    pub async fn track_uri(
        &self,
        source: &SourceId,
        object: &str,
    ) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT media_uri FROM tracks WHERE source_key={SOURCE_KEY} AND object_id=?2"
        )))
        .bind(source.as_str())
        .bind(object)
        .fetch_optional(&mut *connection)
        .await?)
    }

    /// The source id and object id of each track of `uris` that has both, by URI.
    pub async fn track_objects(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<(String, SourceId, String)>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT track.media_uri,source.object_id,track.object_id FROM json_each(?1) requested
             JOIN tracks track ON track.media_uri=requested.value JOIN sources source USING(source_key)
             WHERE track.object_id IS NOT NULL",
        )
        .bind(serde_json::to_string(uris)?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(uri, source, object)| (uri, SourceId::new(source), object))
            .collect())
    }
}
