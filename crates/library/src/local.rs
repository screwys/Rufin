//! Persists bounded Local observations, dependency paths, and point playback access.
//! Walking, parsing, retries, and filesystem policy remain owned by Sources.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite};

use crate::{
    Database, LibraryError, LibraryResult, LocalAccessFileKey, LocalFileKey, ReadCancellation,
    SourceKey, TrackKey,
};

const LOCAL_FILE_PAGE_LIMIT: usize = 128;

pub(crate) fn whole_file_cue_sql(alias: &str) -> String {
    format!("{alias}.indexed=1 AND {alias}.cue_path IS NOT NULL AND {alias}.source_path IS NOT NULL AND {alias}.cue_start_millis=0
        AND NOT EXISTS(SELECT 1 FROM tracks sibling INDEXED BY tracks_source_path_idx
            WHERE sibling.source_key={alias}.source_key AND sibling.source_path={alias}.source_path
              AND sibling.cue_path={alias}.cue_path AND sibling.indexed=1 AND sibling.track_key<>{alias}.track_key)")
}

#[derive(Clone, Debug)]
pub enum LocalMediaLocation {
    File(std::path::PathBuf),
    Document(String),
}

#[derive(Debug, FromRow)]
pub struct ObservedMediaFile {
    pub path: String,
    pub revision: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
}

#[derive(Debug, FromRow)]
pub struct DownloadMetadata {
    pub channels: Option<u32>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
    pub source_format: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub struct FileEditRow {
    pub track_key: TrackKey,
    pub object_id: Option<String>,
    pub media_uri: String,
    pub source_path: Option<String>,
    pub source_format: Option<String>,
    pub cue_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum LocalFileKind {
    Media,
    Cue,
    Image,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum LocalFileState {
    Accepted,
    Rejected,
    Unreadable,
    Observed,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalFileWrite {
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub kind: LocalFileKind,
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub native_id: Option<String>,
    pub revision: Option<String>,
    pub picture_index: Option<i64>,
    pub artist_pictures: Option<String>,
    pub parse_version: Option<i64>,
    pub state: LocalFileState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalFileRow {
    pub local_file_key: LocalFileKey,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub kind: LocalFileKind,
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub native_id: Option<String>,
    pub revision: Option<String>,
    pub picture_index: Option<i64>,
    pub artist_pictures: Option<String>,
    pub parse_version: Option<i64>,
    pub state: LocalFileState,
    pub dependencies: Vec<String>,
    pub track_object_id: Option<String>,
    pub current_output: bool,
    pub imported: bool,
}

#[derive(Clone, Debug, FromRow)]
struct LocalFileScalar {
    local_file_key: LocalFileKey,
    path: String,
    root: String,
    relative_path: String,
    kind: LocalFileKind,
    size_bytes: Option<i64>,
    mtime_ns: i64,
    device_id: Option<i64>,
    inode: Option<i64>,
    native_id: Option<String>,
    revision: Option<String>,
    picture_index: Option<i64>,
    artist_pictures: Option<String>,
    parse_version: Option<i64>,
    state: LocalFileState,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, FromRow)]
pub struct LocalLocatorWrite {
    pub source_id: Option<String>,
    pub media_uri: String,
    pub origin: String,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub access_uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAccessWrite {
    pub media_uri: String,
    pub origin: LocalAccessOrigin,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parser_version: i64,
    pub access_uri: String,
    pub loudness_analysis_key: Option<[u8; 32]>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MappingTrackRow {
    pub track_key: TrackKey,
    pub media_uri: String,
    pub source_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum LocalAccessOrigin {
    Local,
    Mapping,
    Download,
    Import,
}

impl LocalAccessOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Mapping => "mapping",
            Self::Download => "download",
            Self::Import => "import",
        }
    }
}

impl Database {
    /// Uses the saved file inventory for the shared source folder browser.
    pub async fn local_folder_contents(
        &self,
        source: SourceKey,
        parent: Option<&str>,
        configured_roots: &[String],
    ) -> LibraryResult<(Vec<(String, String)>, Vec<String>)> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let mut transaction = connection.begin().await?;
        let roots_json = serde_json::to_string(configured_roots)?;
        let mut parent = parent.map(str::to_owned);
        if parent.is_none() {
            let mut roots: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT root FROM observed_local_files WHERE source_key=?1 AND kind='directory' AND root IN(SELECT value FROM json_each(?2))",
            )
            .bind(source).bind(&roots_json)
            .fetch_all(&mut *transaction)
            .await?;
            let tracks: Vec<String> = sqlx::query_scalar("SELECT track.object_id FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND track.indexed=1 AND NOT EXISTS(SELECT 1 FROM observed_local_files file WHERE file.source_key=track.source_key AND file.path=coalesce(location.cue_path,location.source_path) AND file.relative_path<>'') AND EXISTS(SELECT 1 FROM observed_local_files scope WHERE scope.source_key=track.source_key AND scope.path=coalesce(location.cue_path,location.source_path) AND scope.root IN(SELECT value FROM json_each(?2)))")
                .bind(source).bind(&roots_json).fetch_all(&mut *transaction).await?;
            if roots.len() == 1 && tracks.is_empty() {
                parent = roots.pop();
            } else {
                transaction.commit().await?;
                return Ok((
                    roots
                        .into_iter()
                        .map(|path| {
                            let name = folder_path_name(&path);
                            (path, name)
                        })
                        .collect(),
                    tracks,
                ));
            }
        }
        let parent = parent.expect("single root or requested folder");
        let separator = if url::Url::parse(&parent).is_ok_and(|url| url.has_host()) {
            '/'
        } else {
            std::path::MAIN_SEPARATOR
        };
        let prefix = format!("{}{separator}", parent.trim_end_matches(separator));
        let prefix = crate::query::quote(&prefix);
        let separator = crate::query::quote(&separator.to_string());
        let tail = format!("substr(path,length({prefix})+1)");
        let folders: Vec<(String, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "WITH folders(path) AS (SELECT DISTINCT {prefix}||CASE WHEN instr({tail},{separator})>0 THEN substr({tail},1,instr({tail},{separator})-1) ELSE {tail} END
                FROM observed_local_files WHERE source_key=?1 AND kind='directory' AND root IN(SELECT value FROM json_each(?2))
                  AND path>={prefix} AND path<{prefix}||char(1114111) AND {tail}<>'')
              SELECT folders.path,file.relative_path FROM folders LEFT JOIN observed_local_files file ON file.source_key=?1 AND file.path=folders.path"
        ))).bind(source).bind(&roots_json).fetch_all(&mut *transaction).await?;
        let tracks = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT object_id FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND indexed=1 AND location.source_path>={prefix}
               AND location.source_path<{prefix}||char(1114111)
               AND instr(substr(location.source_path,length({prefix})+1),{separator})=0
               AND EXISTS(SELECT 1 FROM observed_local_files scope WHERE scope.source_key=track.source_key AND scope.path=coalesce(location.cue_path,location.source_path) AND scope.root IN(SELECT value FROM json_each(?2)))"
        )))
        .bind(source).bind(&roots_json)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok((
            folders
                .into_iter()
                .map(|(path, relative)| {
                    let name = folder_path_name(
                        relative
                            .as_deref()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(&path),
                    );
                    (path, name)
                })
                .collect(),
            tracks,
        ))
    }

    /// Lexical endpoints are sufficient to find the common directory of all
    /// member paths. Keep the result bounded even for a very large artist.
    pub async fn collection_source_path_bounds(
        &self,
        media_uri: &str,
        album_artist: bool,
        folder: Option<crate::FolderKey>,
    ) -> LibraryResult<Option<(String, String)>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let members = match crate::source_entity_parts(media_uri).map(|(_, kind, _)| kind) {
            Some(kind) if kind == "album" => {
                "SELECT track.track_key,track.source_key,track.source_path FROM albums album JOIN tracks track USING(album_key) WHERE album.media_uri=?1".to_owned()
            }
            Some(kind) if kind == "artist" && album_artist => {
                format!("SELECT track.track_key,track.source_key,track.source_path FROM artists artist JOIN tracks track ON {} WHERE artist.media_uri=?1", crate::collections::album_artist_member("track", "artist.artist_key"))
            }
            Some(kind) if kind == "artist" => {
                "SELECT track.track_key,track.source_key,track.source_path FROM artists artist JOIN track_artists credit USING(artist_key) JOIN tracks track USING(track_key) WHERE artist.media_uri=?1".to_owned()
            }
            _ => return Ok(None),
        };
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT min(path),max(path) FROM (SELECT member.track_key,CASE WHEN (SELECT kind FROM sources WHERE source_key=member.source_key)='local' THEN(SELECT access.source_path FROM local_track_access access WHERE access.track_key=member.track_key) ELSE member.source_path END path FROM (",
        );
        query.push(&members).push(") member) paths WHERE path<>'' AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=paths.track_key AND scope.folder_key=?2))");
        let (first, last): (Option<String>, Option<String>) = query
            .build_query_as()
            .bind(media_uri)
            .bind(folder)
            .fetch_one(&mut *connection)
            .await?;
        Ok(first.zip(last))
    }

    pub async fn file_edit_rows(
        &self,
        collection_uri: &str,
        after: Option<crate::TrackKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FileEditRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let members = match crate::source_entity_parts(collection_uri).map(|(_, kind, _)| kind) {
            Some(kind) if kind == "album" => {
                "SELECT track.track_key FROM albums album JOIN tracks track USING(album_key) WHERE album.media_uri=?1 AND track.track_key>coalesce(?2,0) ORDER BY track.track_key LIMIT 128".to_owned()
            }
            Some(kind) if kind == "artist" => {
                format!("SELECT track_key FROM tracks WHERE track_key>coalesce(?2,0) AND track_key IN(SELECT relation.track_key FROM artists artist JOIN track_artists relation USING(artist_key) WHERE artist.media_uri=?1 UNION SELECT track.track_key FROM artists artist JOIN tracks track ON {} WHERE artist.media_uri=?1) ORDER BY track_key LIMIT 128", crate::collections::album_artist_member("track", "artist.artist_key"))
            }
            _ => "SELECT track_key FROM tracks WHERE media_uri=?1 AND track_key>coalesce(?2,0)".to_owned(),
        };
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("WITH members AS({members}) SELECT track.track_key,track.object_id,track.media_uri,CASE WHEN (SELECT kind FROM sources WHERE source_key=track.source_key)='local' THEN(SELECT access.source_path FROM local_track_access access WHERE access.track_key=track.track_key) ELSE track.source_path END source_path,track.source_format,coalesce((SELECT access.cue_path FROM local_track_access access WHERE access.track_key=track.track_key),track.cue_path) cue_path FROM members JOIN tracks track USING(track_key) ORDER BY track.track_key")))
            .bind(collection_uri).bind(after).fetch_all(&mut *connection).await?)
    }

    pub async fn original_files(
        &self,
        media_uris: &[String],
    ) -> LibraryResult<Vec<LocalLocatorWrite>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("WITH ranked AS(SELECT source.object_id source_id,locator.*,row_number() OVER(PARTITION BY media_uri ORDER BY CASE origin WHEN 'mapping' THEN 0 ELSE 1 END,local_access_file_key) choice FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE media_uri IN(SELECT value FROM json_each(?1)) AND origin<>'download') SELECT source_id,media_uri,origin,path,root,relative_path,access_uri FROM ranked WHERE choice=1")
            .bind(serde_json::to_string(media_uris)?).fetch_all(&mut *connection).await?)
    }

    pub async fn media_uris_for_document_ids(
        &self,
        ids: &[String],
    ) -> LibraryResult<Vec<Option<String>>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT (
                SELECT track.media_uri FROM sources source
                CROSS JOIN observed_local_files file ON file.source_key=source.source_key AND file.native_id=requested.value
                JOIN local_file_access_roots access ON access.source_key=file.source_key AND access.root=file.root
                JOIN local_track_access location ON location.source_key=file.source_key AND location.source_path=file.path
                JOIN tracks track USING(track_key)
                WHERE location.cue_path IS NULL AND track.cue_start_millis IS NULL
                ORDER BY track.indexed DESC,track.track_key LIMIT 1
             ) FROM json_each(?1) requested ORDER BY requested.key",
        )
        .bind(serde_json::to_string(ids)?)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn saved_file_observation(
        &self,
        source: SourceKey,
        media_uri: &str,
    ) -> LibraryResult<Option<(String, Option<String>, Option<LocalFileWrite>)>> {
        let mut connection = self.acquire_reader().await?;
        let row:Option<(String,Option<String>,Option<String>)>=sqlx::query_as("SELECT track.object_id,track.source_path,CASE WHEN file.local_file_key IS NOT NULL THEN json_object('path',file.path,'root',file.root,'relative_path',file.relative_path,'kind',file.kind,'size_bytes',file.size_bytes,'mtime_ns',file.mtime_ns,'device_id',file.device_id,'inode',file.inode,'native_id',file.native_id,'revision',file.revision,'picture_index',file.picture_index,'artist_pictures',file.artist_pictures,'parse_version',file.parse_version,'state',file.state) END FROM tracks track LEFT JOIN local_track_access location USING(track_key) LEFT JOIN observed_local_files file ON file.source_key=track.source_key AND file.path=location.source_path WHERE track.source_key=?1 AND track.media_uri=?2")
            .bind(source).bind(media_uri).fetch_optional(&mut *connection).await?;
        row.map(|(id, path, file)| {
            Ok((
                id,
                path,
                file.map(|json| serde_json::from_str(&json)).transpose()?,
            ))
        })
        .transpose()
    }

    pub async fn downloaded_count(&self, source_id: Option<&str>) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let count = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key=(SELECT source_key FROM main.sources WHERE object_id=?1)
                   AND origin='download'",
            )
            .bind(source_id)
            .fetch_one(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key IS NULL AND origin='download'",
            )
            .fetch_one(&mut *connection)
            .await
        };
        Ok(usize::try_from(count?).unwrap_or_default())
    }

    pub async fn downloaded_media_count(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT count(DISTINCT media_uri) FROM main.local_locators WHERE origin='download' AND media_uri IN (SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(media_uris)?).fetch_one(&mut *connection).await?)
    }

    pub async fn downloaded_media_uris(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT DISTINCT media_uri FROM main.local_locators WHERE origin='download' AND media_uri IN (SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(media_uris)?).fetch_all(&mut *connection).await?)
    }

    pub async fn download_metadata(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<DownloadMetadata>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT COALESCE(title,media_uri) title,COALESCE(display_artist,'') artist,COALESCE(display_album,'') album,COALESCE(disc_number,0) disc_number,COALESCE(track_number,0) track_number,
                    COALESCE(duration_millis,0) duration_millis,source_format,json_extract(metadata,'$.audio_properties.channels') channels
             FROM tracks WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn playback_access(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<(String, LocalAccessOrigin)>> {
        let mut connection = self.acquire_reader().await?;
        let access = sqlx::query_as::<_, (String, LocalAccessOrigin, bool)>(
            "SELECT location,origin,is_path FROM (
                SELECT access_uri location,origin,0 is_path,
                    CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END priority,
                    local_access_file_key key
                FROM local_access_files WHERE media_uri=?1
                UNION ALL
                SELECT location.source_path,'local',1,3,track.track_key FROM tracks track JOIN sources source USING(source_key) JOIN local_track_access location USING(track_key)
                WHERE media_uri=?1 AND source.kind='local'
             ) ORDER BY priority,key LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        Ok(access.and_then(|(location, origin, is_path)| {
            if is_path {
                crate::document_access_uri(&location)
                    .or_else(|| location.starts_with("content:").then(|| location.clone()))
                    .or_else(|| url::Url::from_file_path(location).ok().map(String::from))
                    .map(|uri| (uri, origin))
            } else {
                Some((location, origin))
            }
        }))
    }

    /// Paths admitted on this device, including downloaded and mapped copies.
    pub async fn local_media_locations(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<(String, Vec<LocalMediaLocation>, bool)>> {
        let backings = uris
            .iter()
            .map(|uri| {
                crate::cue_media_parts(uri)
                    .map(|(_, backing, _, _)| backing)
                    .filter(|backing| crate::document_media_id(backing).is_some())
            })
            .collect::<Vec<_>>();
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, bool, bool)> = sqlx::query_as(
            "SELECT requested.value,
                (SELECT json_group_array(path) FROM (
                    SELECT CASE WHEN access_uri LIKE 'content:%' THEN access_uri ELSE path END AS path FROM main.local_locators WHERE media_uri IN (requested.value,json_extract(?2,'$[' || requested.key || ']'))
                    UNION SELECT location.source_path FROM main.tracks track JOIN sources source USING(source_key) JOIN local_track_access location USING(track_key)
                      WHERE media_uri=requested.value AND source.kind='local')),
                EXISTS(SELECT 1 FROM tracks track WHERE track.media_uri=requested.value AND track.source_path IS NOT NULL AND NOT EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=track.track_key) AND NOT EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin IN ('local','import'))),
                EXISTS(SELECT 1 FROM playlist_entries entry JOIN tracks track USING(track_key) WHERE track.media_uri=requested.value)
                    OR EXISTS(SELECT 1 FROM queue_entries entry JOIN tracks track USING(track_key) WHERE track.media_uri=requested.value AND NOT entry.received_from_connect)
             FROM json_each(?1) requested",
        )
        .bind(serde_json::to_string(uris)?)
        .bind(serde_json::to_string(&backings)?)
        .fetch_all(&mut *connection).await?;
        rows.into_iter()
            .map(|(uri, paths, shared, direct)| {
                let paths: Vec<String> = serde_json::from_str(&paths)?;
                let mut paths = paths
                    .into_iter()
                    .map(|path| {
                        if path.starts_with("content:") {
                            Ok(LocalMediaLocation::Document(path))
                        } else if path.starts_with("rufin-document:") {
                            crate::document_access_uri(&path)
                                .map(LocalMediaLocation::Document)
                                .ok_or(LibraryError::InvalidRequest(
                                    "Document locator has no access URI".into(),
                                ))
                        } else {
                            Ok(LocalMediaLocation::File(path.into()))
                        }
                    })
                    .collect::<LibraryResult<Vec<_>>>()?;
                // A direct import can be checked before its metadata has been read.
                if direct && !shared {
                    let backing = crate::cue_media_parts(&uri).map(|(_, backing, _, _)| backing);
                    if let Some(path) = crate::file_media_path(backing.as_deref().unwrap_or(&uri)) {
                        paths.push(LocalMediaLocation::File(path));
                    } else if let Some(uri) = backing
                        .as_deref()
                        .or(Some(uri.as_str()))
                        .filter(|uri| uri.starts_with("content:"))
                    {
                        paths.push(LocalMediaLocation::Document(uri.to_owned()));
                    }
                }
                Ok((uri, paths, shared))
            })
            .collect()
    }

    pub async fn catalog_missing_media(
        &self,
        media: &[(String, crate::SourceId)],
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT json_extract(requested.value,'$[0]') FROM json_each(?1) requested
            WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=json_extract(requested.value,'$[1]') AND catalog_complete=1)
              AND NOT EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=json_extract(requested.value,'$[0]') AND indexed=1)")
            .bind(serde_json::to_string(media)?).fetch_all(&mut *connection).await?)
    }

    /// A native scan establishes local access even if the URI is also shared.
    pub async fn file_media_is_remote(
        &self,
        media_uri: &str,
        received_from_connect: bool,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT
            NOT EXISTS(SELECT 1 FROM tracks track JOIN local_track_access access USING(track_key) WHERE media_uri=?1)
            AND (EXISTS(SELECT 1 FROM tracks track WHERE track.media_uri=?1 AND track.source_path IS NOT NULL) OR ?2)",
        )
        .bind(media_uri)
        .bind(received_from_connect)
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn playback_window(&self, uri: &str) -> LibraryResult<Option<(u64, u64)>> {
        let mut connection = self.acquire_reader().await?;
        let whole_file = whole_file_cue_sql("track");
        let window: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT track.cue_start_millis,CASE WHEN {whole_file} THEN NULL ELSE track.cue_end_millis END FROM tracks track WHERE media_uri=?1")))
            .bind(uri).fetch_optional(&mut *connection).await?;
        if let Some((Some(start), end)) = window {
            if start == 0 && end.is_none() {
                return Ok(None);
            }
            if let Some(end) = end.filter(|end| start >= 0 && *end > start) {
                return Ok(Some((start as u64, end as u64)));
            }
        }
        Ok(crate::cue_media_parts(uri).map(|(_, _, start, end)| (start as u64, end as u64)))
    }

    pub async fn observed_media_file(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<ObservedMediaFile>> {
        let mut connection = self.acquire_reader().await?;
        let whole_file = whole_file_cue_sql("track");
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(location.source_path,canonical.path) path,
                    CASE WHEN location.source_path IS NOT NULL THEN file.revision ELSE canonical.revision END revision,track.cue_start_millis,
                    CASE WHEN {whole_file} THEN NULL ELSE track.cue_end_millis END cue_end_millis
             FROM tracks track JOIN sources source USING(source_key)
             LEFT JOIN local_track_access location USING(track_key)
             LEFT JOIN observed_local_files file ON file.source_key=track.source_key AND file.path=location.source_path
             LEFT JOIN local_files canonical ON canonical.source_key=track.source_key AND canonical.path=track.source_path
             WHERE track.media_uri=?1 AND(location.source_path IS NOT NULL OR(source.kind IN('webdav','smb') AND canonical.path IS NOT NULL))"
        )))
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn media_uri_for_file_path(
        &self,
        source: &str,
        path: &str,
    ) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM tracks track JOIN sources USING(source_key) JOIN local_track_access access USING(track_key) WHERE sources.object_id=?1 AND access.source_path=?2 AND track.cue_path IS NULL LIMIT 1")
            .bind(source).bind(path).fetch_optional(&mut *connection).await?)
    }

    pub async fn source_counts(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let(album,track)=sqlx::query_as::<_,(i64,i64)>("SELECT (SELECT count(*) FROM albums album WHERE source_key=?1 AND (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))),(SELECT count(*) FROM tracks WHERE source_key=?1 AND indexed=1)").bind(source).fetch_one(&mut *connection).await?;
        Ok((
            usize::try_from(album).unwrap_or_default(),
            usize::try_from(track).unwrap_or_default(),
        ))
    }

    pub async fn all_source_counts(
        &self,
    ) -> LibraryResult<std::collections::HashMap<crate::SourceId, (usize, usize)>> {
        let mut connection = self.acquire_reader().await?;
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT object_id,
                (SELECT count(*) FROM albums album WHERE source_key=source.source_key AND (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))),
                (SELECT count(*) FROM tracks WHERE source_key=source.source_key AND indexed=1)
             FROM sources source",
        )
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, albums, tracks)| {
                (crate::SourceId::new(id), (albums as usize, tracks as usize))
            })
            .collect())
    }

    pub async fn mapping_formula_match_count(
        &self,
        source: SourceKey,
        root: &str,
        server_prefix: Option<&str>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks
             WHERE source_key=?1 AND indexed=1 AND source_path IS NOT NULL AND (
               (?2 IS NOT NULL AND substr(source_path,1,length(?2))=?2)
               OR (?2 IS NULL AND (
                 (substr(source_path,1,1)<>'/' AND substr(source_path,2,1)<>':' AND substr(source_path,1,2)<>char(92)||char(92))
                 OR substr(source_path,1,length(?3))=?3
               ))
             )",
        )
        .bind(source)
        .bind(server_prefix)
        .bind(root)
        .fetch_one(&mut *connection)
        .await?;
        Ok(usize::try_from(count).unwrap_or_default())
    }

    pub async fn clear_mapping_access(&self, source: SourceKey) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM main.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM local_access_files WHERE source_key=?1 AND origin='mapping') AND NOT EXISTS(SELECT 1 FROM local_locators other WHERE other.access_uri=local_access_metadata.access_uri AND (other.source_key IS NOT ?1 OR other.origin<>'mapping'))").bind(source).execute(&mut *connection).await?;
        let removed =
            sqlx::query("DELETE FROM main.local_locators WHERE local_access_file_key IN (SELECT local_access_file_key FROM local_access_files WHERE source_key=?1 AND origin='mapping')")
                .bind(source)
                .execute(&mut *connection)
                .await?
                .rows_affected();
        sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key) WHERE source_key=?1")
            .bind(source).execute(&mut *connection).await?;
        Ok(removed)
    }

    pub async fn mapping_track_page(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        source_path: Option<&str>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MappingTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as::<_, MappingTrackRow>(
            "SELECT track_key,media_uri,source_path
             FROM tracks WHERE source_key=?1 AND indexed=1 AND source_path IS NOT NULL
               AND ((?3 IS NULL AND track_key>?2) OR source_path=?3)
             ORDER BY track_key LIMIT ?4",
        )
        .bind(source)
        .bind(after.map_or(0, TrackKey::raw))
        .bind(source_path)
        .bind(limit.clamp(1, 128) as i64)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn mapping_track_source_path(
        &self,
        source: SourceKey,
        track: TrackKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT source_path FROM tracks
             WHERE source_key=?1 AND track_key=?2 AND source_path IS NOT NULL",
        )
        .bind(source)
        .bind(track)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn local_file_roots(&self, source: SourceKey) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT root FROM local_file_access_roots WHERE source_key=?1",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn local_file_page(
        &self,
        source: SourceKey,
        after: Option<LocalFileKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        let limit = limit.clamp(1, LOCAL_FILE_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let scalars = sqlx::query_as::<_, LocalFileScalar>("SELECT local_file_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state FROM observed_local_files WHERE source_key=?1 AND EXISTS(SELECT 1 FROM local_file_access_roots access WHERE access.source_key=observed_local_files.source_key AND access.root=observed_local_files.root) AND local_file_key>?2 ORDER BY local_file_key LIMIT ?3")
            .bind(source).bind(after.map_or(0, LocalFileKey::raw)).bind(limit)
            .fetch_all(&mut *transaction).await?;
        let rows = load_local_file_rows(&mut transaction, source, scalars).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn write_local_access(
        &self,
        source: Option<SourceKey>,
        records: &[&LocalAccessWrite],
    ) -> LibraryResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        write_local_access_on(&mut transaction, source, records).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn original_file(&self, media_uri: &str) -> LibraryResult<Option<LocalLocatorWrite>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_as("SELECT source.object_id source_id,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM main.local_locators locator LEFT JOIN main.sources source USING(source_key) WHERE media_uri=?1 AND origin<>'download' ORDER BY CASE origin WHEN 'mapping' THEN 0 ELSE 1 END,local_access_file_key LIMIT 1")
            .bind(media_uri).fetch_optional(&mut *connection).await?)
    }

    pub async fn remove_local_access(&self, key: LocalAccessFileKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let removed_access = sqlx::query_scalar::<_, String>(
            "SELECT media_uri FROM local_access_files WHERE local_access_file_key=?1",
        )
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM main.local_access_metadata WHERE access_uri=(SELECT access_uri FROM main.local_locators WHERE local_access_file_key=?1) AND NOT EXISTS(SELECT 1 FROM local_locators other WHERE other.access_uri=local_access_metadata.access_uri AND other.local_access_file_key<>?1)")
            .bind(key)
            .execute(&mut *transaction)
            .await?;
        let removed = sqlx::query("DELETE FROM main.local_locators WHERE local_access_file_key=?1")
            .bind(key)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        if let Some(media_uri) = removed_access.as_deref() {
            refresh_access_audio(&mut transaction, &[media_uri]).await?;
        }
        transaction.commit().await?;
        Ok(removed)
    }
}

async fn refresh_access_audio(
    connection: &mut sqlx::SqliteConnection,
    uris: &[&str],
) -> LibraryResult<()> {
    let chosen = "coalesce((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key)";
    let albums = sqlx::query_scalar::<_, Option<crate::AlbumKey>>(sqlx::AssertSqlSafe(format!(
        "UPDATE tracks SET loudness_analysis_key={chosen} WHERE media_uri IN (SELECT value FROM json_each(?1)) AND loudness_analysis_key IS NOT {chosen} RETURNING album_key"
    ))).bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
    let albums = albums
        .into_iter()
        .flatten()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    crate::loudness::invalidate_album_audio(connection, &albums, false).await?;
    Ok(())
}

fn folder_path_name(path: &str) -> String {
    if let Some(url) = url::Url::parse(path).ok().filter(|url| url.has_host()) {
        url.path()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(|name| {
                percent_encoding::percent_decode_str(name)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| url.host_str().unwrap_or(path).to_string())
    } else {
        std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
    }
}

pub(crate) async fn local_file_reuse_candidates_on(
    connection: &mut sqlx::SqliteConnection,
    source: SourceKey,
    files: &[LocalFileWrite],
) -> LibraryResult<Vec<LocalFileRow>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let input = serde_json::to_string(files)?;
    let scalars=sqlx::query_as::<_,LocalFileScalar>("WITH requested AS MATERIALIZED(SELECT CAST(key AS INTEGER) position,value file FROM json_each(?2)),roots AS MATERIALIZED(
        SELECT requested.position,access.root_id FROM requested JOIN local_file_access_roots access ON access.source_key=?1 AND access.root=json_extract(requested.file,'$.root')
        UNION SELECT requested.position,file.root_id FROM requested JOIN local_files file ON file.source_key=?1 AND file.root=json_extract(requested.file,'$.root') AND file.relative_path=json_extract(requested.file,'$.relative_path')
        WHERE NOT EXISTS(SELECT 1 FROM local_file_access_roots access WHERE access.source_key=?1 AND access.root=json_extract(requested.file,'$.root'))),candidates AS(
        SELECT requested.position,file.local_file_key,file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.native_id,file.revision,file.picture_index,file.artist_pictures,file.parse_version,file.state
        FROM requested JOIN observed_local_files file ON file.source_key=?1 AND(file.path=json_extract(requested.file,'$.path') OR(file.native_id IS NOT NULL AND file.native_id=json_extract(requested.file,'$.native_id')) OR(file.device_id IS NOT NULL AND file.inode IS NOT NULL AND(file.device_id,file.inode) IS(json_extract(requested.file,'$.device_id'),json_extract(requested.file,'$.inode'))))
        UNION ALL
        SELECT requested.position,file.local_file_key,json_extract(requested.file,'$.path'),json_extract(requested.file,'$.root'),json_extract(requested.file,'$.relative_path'),file.kind,file.size_bytes,file.mtime_ns,json_extract(requested.file,'$.device_id'),json_extract(requested.file,'$.inode'),json_extract(requested.file,'$.native_id'),file.revision,file.picture_index,file.artist_pictures,coalesce(json_extract(file.parsed_stamp,'$[3]'),file.parse_version),file.state
        FROM requested JOIN roots USING(position) CROSS JOIN local_files file ON file.source_key=?1 AND file.root_id=roots.root_id AND file.relative_path=json_extract(requested.file,'$.relative_path')
        WHERE NOT EXISTS(SELECT 1 FROM local_file_observations access WHERE access.local_file_key=file.local_file_key) AND(file.size_bytes,file.mtime_ns,file.revision) IS(json_extract(requested.file,'$.size_bytes'),json_extract(requested.file,'$.mtime_ns'),json_extract(requested.file,'$.revision')))
        SELECT local_file_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state FROM candidates ORDER BY position,local_file_key")
        .bind(source).bind(input).fetch_all(&mut *connection).await?;
    load_local_file_rows(connection, source, scalars).await
}

async fn load_local_file_rows(
    connection: &mut sqlx::SqliteConnection,
    source: SourceKey,
    scalars: Vec<LocalFileScalar>,
) -> LibraryResult<Vec<LocalFileRow>> {
    let mut dependencies = BTreeMap::<LocalFileKey, Vec<String>>::new();
    let mut track_objects = BTreeMap::<LocalFileKey, (Option<String>, bool, bool)>::new();
    if !scalars.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,position) AS (");
        query.push_values(scalars.iter().enumerate(), |mut row, (position, file)| {
            row.push_bind(file.local_file_key)
                .push_bind(position as i64);
        });
        query.push(") SELECT dependency.local_file_key,dependency.dependency_path FROM requested JOIN observed_file_dependencies dependency USING(local_file_key) ORDER BY requested.position,dependency.position");
        for (file, dependency) in query
            .build_query_as::<(LocalFileKey, String)>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?
        {
            dependencies.entry(file).or_default().push(dependency);
        }
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,kind) AS(");
        query.push_values(&scalars, |mut row, file| {
            row.push_bind(file.local_file_key).push_bind(file.kind);
        });
        query.push(") SELECT requested.local_file_key,min(CASE WHEN requested.kind='cue' OR track.cue_path IS NULL THEN track.object_id END),max(CASE WHEN requested.kind='cue' OR track.cue_path IS NULL THEN 1 ELSE 0 END),max(track.indexed=1 AND EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import')) FROM requested JOIN tracks track ON track.local_file_key=requested.local_file_key AND track.source_key=")
            .push_bind(source).push(" GROUP BY requested.local_file_key");
        for (file, object_id, current_output, imported) in query
            .build_query_as::<(LocalFileKey, Option<String>, bool, bool)>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?
        {
            track_objects.insert(file, (object_id, current_output, imported));
        }
    }
    Ok(scalars
        .into_iter()
        .map(|scalar| {
            let (track_object_id, current_output, imported) = track_objects
                .remove(&scalar.local_file_key)
                .unwrap_or((None, false, false));
            LocalFileRow {
                local_file_key: scalar.local_file_key,
                path: scalar.path,
                root: scalar.root,
                relative_path: scalar.relative_path,
                kind: scalar.kind,
                size_bytes: scalar.size_bytes,
                mtime_ns: scalar.mtime_ns,
                device_id: scalar.device_id,
                inode: scalar.inode,
                native_id: scalar.native_id,
                revision: scalar.revision,
                picture_index: scalar.picture_index,
                artist_pictures: scalar.artist_pictures,
                parse_version: scalar.parse_version,
                state: scalar.state,
                dependencies: dependencies
                    .remove(&scalar.local_file_key)
                    .unwrap_or_default(),
                track_object_id,
                current_output,
                imported,
            }
        })
        .collect())
}

fn validate_local_access(access: &LocalAccessWrite) -> LibraryResult<()> {
    if access.path.is_empty()
        || access.root.is_empty()
        || access.media_uri.is_empty()
        || access.access_uri.is_empty()
        || access.size_bytes < 0
        || access.parser_version < 1
    {
        return Err(LibraryError::InvalidRequest(
            "invalid Local access file".to_string(),
        ));
    }
    Ok(())
}

impl Database {
    pub async fn upsert_local_locator(
        &self,
        locator: &LocalLocatorWrite,
    ) -> LibraryResult<LocalAccessFileKey> {
        let mut writer = self.writer().await?;
        write_local_locator(
            writer.as_mut().ok_or(LibraryError::WriterUnavailable)?,
            locator,
        )
        .await
    }

    pub async fn import_local_locators_jsonl(
        &self,
        input: impl std::io::BufRead,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let count = import_local_locators_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(count)
    }
}

pub(crate) async fn write_local_locator(
    connection: &mut sqlx::SqliteConnection,
    locator: &LocalLocatorWrite,
) -> LibraryResult<LocalAccessFileKey> {
    crate::catalog::remember_track(
        connection,
        &crate::TrackReference {
            media_uri: locator.media_uri.clone(),
            ..Default::default()
        },
    )
    .await?;
    let source = match &locator.source_id {
        Some(id) => Some(crate::db::write_source_identity(connection, id).await?),
        None => None,
    };
    Ok(sqlx::query_scalar("INSERT INTO main.local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(media_uri,origin) DO UPDATE SET source_key=excluded.source_key,path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING local_access_file_key")
        .bind(source).bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(connection).await?)
}

pub(crate) async fn export_local_locators_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, LocalLocatorWrite>("SELECT source.object_id source_id,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM main.local_locators locator LEFT JOIN main.sources source USING(source_key) WHERE locator.origin<>'download' ORDER BY locator.local_access_file_key").fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}
pub(crate) async fn import_local_locators_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    read_local_locators(connection, input, false).await
}

pub(crate) async fn import_local_locators_jsonl_preserving_downloads_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    read_local_locators(connection, input, true).await
}

async fn read_local_locators(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
    preserve_downloads: bool,
) -> LibraryResult<u64> {
    let mut lines = input.lines();
    let header = lines
        .next()
        .transpose()?
        .ok_or_else(|| LibraryError::InvalidRequest("missing locator format version".into()))?;
    if serde_json::from_str::<serde_json::Value>(&header)?
        .get("version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(LibraryError::InvalidRequest(
            "unsupported locator format version".into(),
        ));
    }
    let mut count = 0;
    for line in lines {
        let locator = serde_json::from_str::<LocalLocatorWrite>(&line?)?;
        if preserve_downloads && locator.origin=="download" && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM main.local_locators WHERE origin='download' AND media_uri=?1)").bind(&locator.media_uri).fetch_one(&mut *connection).await? { continue; }
        write_local_locator(connection, &locator).await?;
        count += 1;
    }
    Ok(count)
}

impl Database {
    pub async fn imported_local_uri_page(
        &self,
        playlist: Option<crate::PlaylistKey>,
        after: &str,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_as(
            "SELECT imported.media_uri,(SELECT access.cue_path FROM local_track_access access WHERE access.track_key=track.track_key AND access.local_file_key IS NOT NULL) FROM (SELECT track.media_uri FROM playlist_entries entry JOIN tracks track USING(track_key)
            WHERE (?1 IS NULL OR entry.playlist_key=?1) AND track.media_uri>?2
              AND (track.media_uri LIKE 'file:%' OR track.media_uri LIKE 'rufin:cue/%' OR track.media_uri LIKE 'rufin:document/%' OR track.media_uri LIKE 'content:%')
            UNION SELECT media_uri FROM user_media_state WHERE ?1 IS NULL AND media_uri>?2
              AND (favorite=1 OR rating IS NOT NULL)
              AND (media_uri LIKE 'file:%' OR media_uri LIKE 'rufin:cue/%' OR media_uri LIKE 'rufin:document/%' OR media_uri LIKE 'content:%')
            UNION SELECT media_uri FROM local_locators WHERE ?1 IS NULL AND origin='import' AND media_uri>?2
              AND(media_uri LIKE 'file:%' OR media_uri LIKE 'rufin:cue/%' OR media_uri LIKE 'rufin:document/%' OR media_uri LIKE 'content:%')
            ) imported LEFT JOIN tracks track USING(media_uri)
            WHERE EXISTS(SELECT 1 FROM local_track_access access WHERE access.track_key=track.track_key) OR track.source_path IS NULL
              OR EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=imported.media_uri AND locator.origin='import')
            ORDER BY imported.media_uri LIMIT 128",
        )
        .bind(playlist)
        .bind(after)
        .fetch_all(&mut *connection)
        .await?)
    }
}

impl crate::Catalog<'_> {
    pub async fn record_imported_files(
        &mut self,
        playlist: Option<crate::PlaylistKey>,
    ) -> LibraryResult<()> {
        let mut after = -1_i64;
        loop {
            let mut writer = self.database.writer().await?;
            let mut tx = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            let (owner, cursor, references) = if playlist.is_some() {
                (
                    "playlist_entries requested CROSS JOIN tracks track USING(track_key)",
                    "requested.position",
                    "requested.playlist_key=?3 AND requested.position>?2",
                )
            } else {
                (
                    "tracks track NOT INDEXED",
                    "track.track_key",
                    "track.track_key>?2 AND(EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key) OR EXISTS(SELECT 1 FROM user_media_state state WHERE state.media_uri=track.media_uri AND(state.favorite=1 OR state.rating IS NOT NULL))) AND ?3 IS NULL",
                )
            };
            let rows: Vec<(i64,String,String,String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {cursor} ordinal,track.media_uri,location.source_path,file.root
                 FROM {owner} JOIN local_track_access location USING(track_key) JOIN observed_local_files file ON file.local_file_key=location.local_file_key
                 WHERE track.source_key=?1 AND track.indexed=1 AND track.source_path IS NOT NULL
                 AND NOT EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import')
                 AND {references}
                 ORDER BY ordinal LIMIT 128"
            ))).bind(self.source).bind(after).bind(playlist).fetch_all(&mut *tx).await?;
            let Some(last) = rows.last() else {
                tx.commit().await?;
                break;
            };
            after = last.0;
            let locators = rows
                .into_iter()
                .filter_map(|(_, uri, path, root)| {
                    url::Url::from_file_path(&path).ok().map(|access| {
                        let relative = std::path::Path::new(&path)
                            .strip_prefix(&root)
                            .unwrap_or(std::path::Path::new(&path))
                            .to_string_lossy()
                            .into_owned();
                        (uri, path, root, relative, access.to_string())
                    })
                })
                .collect::<Vec<_>>();
            if !locators.is_empty() {
                let mut insert = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)",
                );
                insert.push_values(&locators, |mut row, (uri, path, root, relative, access)| {
                    row.push_bind(self.source)
                        .push_bind(uri)
                        .push_bind("import")
                        .push_bind(path)
                        .push_bind(root)
                        .push_bind(relative)
                        .push_bind(access);
                });
                insert
                    .push(" ON CONFLICT(media_uri,origin) DO NOTHING")
                    .build()
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }
}

pub(crate) async fn write_local_access_on(
    connection: &mut sqlx::SqliteConnection,
    source: Option<SourceKey>,
    records: &[&LocalAccessWrite],
) -> LibraryResult<()> {
    for access in records {
        validate_local_access(access)?;
    }
    let source_id = if let Some(source) = source {
        sqlx::query_scalar::<_, String>("SELECT object_id FROM main.sources WHERE source_key=?1")
            .bind(source)
            .fetch_optional(&mut *connection)
            .await?
    } else {
        None
    };
    for access in records {
        sqlx::query("DELETE FROM main.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3) AND NOT EXISTS(SELECT 1 FROM local_locators other WHERE other.access_uri=local_access_metadata.access_uri AND (other.media_uri<>?1 OR other.origin<>?2 OR other.path=?3))").bind(&access.media_uri).bind(access.origin.as_str()).bind(&access.path).execute(&mut *connection).await?;
        sqlx::query(
            "DELETE FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3",
        )
        .bind(&access.media_uri)
        .bind(access.origin.as_str())
        .bind(&access.path)
        .execute(&mut *connection)
        .await?;
        write_local_locator(
            &mut *connection,
            &LocalLocatorWrite {
                source_id: source_id.clone(),
                media_uri: access.media_uri.clone(),
                origin: access.origin.as_str().to_owned(),
                path: access.path.clone(),
                root: access.root.clone(),
                relative_path: access.relative_path.clone(),
                access_uri: access.access_uri.clone(),
            },
        )
        .await?;
        sqlx::query("INSERT INTO main.local_access_metadata(access_uri,size_bytes,mtime_ns,
            device_id,inode,parser_version,loudness_analysis_key) VALUES(?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(access_uri) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,
            device_id=excluded.device_id,inode=excluded.inode,parser_version=excluded.parser_version,
            loudness_analysis_key=excluded.loudness_analysis_key")
            .bind(&access.access_uri).bind(access.size_bytes).bind(access.mtime_ns).bind(access.device_id).bind(access.inode)
            .bind(access.parser_version).bind(access.loudness_analysis_key.as_ref().map(|key|key.as_slice()))
            .execute(&mut *connection).await?;
    }
    refresh_access_audio(
        &mut *connection,
        &records
            .iter()
            .map(|row| row.media_uri.as_str())
            .collect::<Vec<_>>(),
    )
    .await?;
    Ok(())
}
