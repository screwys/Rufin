//! Reads of file-source tracks, downloads and locators. Walking, parsing, retries and
//! filesystem policy remain owned by Sources.

use sqlx::{Connection, FromRow};

use crate::catalog::snapshot::Packed;
use crate::catalog::write::Write;
use crate::{
    Catalog, LibraryError, LibraryResult, LocalAccessFileKey, ReadCancellation, SourceKey, TrackKey,
};

pub(crate) fn whole_file_cue_sql(track: &str, details: &str) -> String {
    format!("{track}.indexed=1 AND {details}.cue_path IS NOT NULL AND {details}.source_path IS NOT NULL AND {details}.cue_start_millis=0
        AND NOT EXISTS(SELECT 1 FROM track_details sibling_details JOIN tracks sibling USING(track_key)
            WHERE sibling_details.source_path={details}.source_path AND sibling_details.cue_path={details}.cue_path
              AND sibling.source_key={track}.source_key AND sibling.indexed=1 AND sibling.track_key<>{track}.track_key)")
}

/// A row read earlier, whose downloads [`Catalog::show_downloads`] shows as they are now.
pub enum Shown<'a> {
    Track(&'a mut crate::TrackRow),
    Album(&'a mut crate::AlbumRow),
    Artist(&'a mut crate::ArtistRow),
    Tag(&'a mut crate::TagRow),
    Folder(&'a mut crate::FolderRow),
    Playlist(&'a mut crate::PlaylistRow),
    Smart(&'a mut crate::SmartPlaylistRow),
}

impl crate::Row {
    /// The row as [`Catalog::show_downloads`] shows its downloads; None for a playlist folder.
    pub fn shown(&mut self) -> Option<Shown<'_>> {
        Some(match self {
            Self::Track(track) | Self::AlbumTrack { track, .. } => Shown::Track(track),
            Self::Album(album) | Self::AlbumHeading(album) => Shown::Album(album),
            Self::Artist(artist) => Shown::Artist(artist),
            Self::Tag(tag) => Shown::Tag(tag),
            Self::Folder(folder) => Shown::Folder(folder),
            Self::Playlist(crate::PlaylistListRow::Playlist(playlist)) => Shown::Playlist(playlist),
            Self::Playlist(crate::PlaylistListRow::SmartPlaylist(smart)) => Shown::Smart(smart),
            Self::Playlist(crate::PlaylistListRow::Folder(_)) => return None,
        })
    }
}

impl crate::Header {
    /// The header as [`Catalog::show_downloads`] shows its downloads.
    pub fn shown(&mut self) -> Shown<'_> {
        match self {
            Self::Album(album) => Shown::Album(&mut album.row),
            Self::Artist(artist) => Shown::Artist(&mut artist.row),
            Self::Playlist(playlist) => Shown::Playlist(playlist),
            Self::Smart(smart) => Shown::Smart(smart),
            Self::Tag(tag) => Shown::Tag(tag),
        }
    }
}

impl Shown<'_> {
    /// Takes the downloads `from` shows when it is the same row read again; false when it is
    /// another row.
    pub fn take_downloads(&mut self, from: Shown<'_>) -> bool {
        match (self, from) {
            (Self::Track(row), Shown::Track(from)) if row.track_key == from.track_key => {
                row.is_downloaded = from.is_downloaded;
            }
            (Self::Album(row), Shown::Album(from)) if row.album_key == from.album_key => {
                row.summary.downloaded_count = from.summary.downloaded_count;
            }
            (Self::Artist(row), Shown::Artist(from))
                if (row.artist_key, row.album_artist) == (from.artist_key, from.album_artist) =>
            {
                row.summary.downloaded_count = from.summary.downloaded_count;
            }
            (Self::Tag(row), Shown::Tag(from)) if row.key == from.key => {
                row.summary.downloaded_count = from.summary.downloaded_count;
            }
            (Self::Folder(row), Shown::Folder(from)) if row.object_id == from.object_id => {
                row.summary.downloaded_count = from.summary.downloaded_count;
            }
            (Self::Playlist(row), Shown::Playlist(from))
                if row.playlist_key == from.playlist_key =>
            {
                row.downloaded_count = from.downloaded_count;
            }
            (Self::Smart(row), Shown::Smart(from))
                if row.smart_playlist_key == from.smart_playlist_key =>
            {
                row.downloaded_count = from.downloaded_count;
            }
            _ => return false,
        }
        true
    }
}

#[derive(Clone, Debug)]
pub enum LocalMediaLocation {
    File(std::path::PathBuf),
    Document(String),
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

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MappingTrackRow {
    pub track_key: TrackKey,
    pub media_uri: String,
    pub source_path: String,
}

impl Catalog {
    /// Uses the saved file inventory for the shared source folder browser.
    pub async fn local_folder_contents(
        &self,
        source: SourceKey,
        parent: Option<&str>,
        configured_roots: &[String],
    ) -> LibraryResult<(Vec<(String, String)>, Vec<String>)> {
        let mut connection = self.acquire_cancellable(&ReadCancellation::new()).await?;
        let mut transaction = connection.begin().await?;
        let roots_json = serde_json::to_string(configured_roots)?;
        let mut parent = parent.map(str::to_owned);
        if parent.is_none() {
            let mut roots: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT root FROM files WHERE source_key=?1 AND kind='directory' AND root IN(SELECT value FROM json_each(?2))",
            )
            .bind(source).bind(&roots_json)
            .fetch_all(&mut *transaction)
            .await?;
            // Single-file roots list their tracks beside the folders.
            let tracks: Vec<String> = sqlx::query_scalar("SELECT track.object_id FROM json_each(?2) root CROSS JOIN files file ON file.source_key=?1 AND file.root=root.value AND replace(file.relative_path,'/',char(1))='' AND file.kind<>'directory' CROSS JOIN track_details details ON details.source_path=file.path OR details.cue_path=file.path CROSS JOIN tracks track ON track.track_key=details.track_key AND track.source_key=?1 AND track.indexed=1")
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
                FROM files WHERE source_key=?1 AND kind='directory' AND root IN(SELECT value FROM json_each(?2))
                  AND path>={prefix} AND path<{prefix}||char(1114111) AND {tail}<>'')
              SELECT folders.path,file.relative_path FROM folders LEFT JOIN files file ON file.source_key=?1 AND file.path=folders.path"
        ))).bind(source).bind(&roots_json).fetch_all(&mut *transaction).await?;
        let tracks = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT object_id FROM track_details details CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND indexed=1 AND details.source_path>={prefix}
               AND details.source_path<{prefix}||char(1114111)
               AND instr(substr(details.source_path,length({prefix})+1),{separator})=0
               AND EXISTS(SELECT 1 FROM files scope WHERE scope.source_key=track.source_key AND scope.path=coalesce(details.cue_path,details.source_path) AND scope.root IN(SELECT value FROM json_each(?2)))"
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
        let mut connection = self.acquire_cancellable(&ReadCancellation::new()).await?;
        let members = self
            .collection_members(&mut connection, media_uri, Some(album_artist), folder)
            .await?;
        let (first, last): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT min(path),max(path) FROM (SELECT CASE WHEN source.kind='local' THEN (SELECT file.path FROM files file WHERE file.source_key=track.source_key AND file.path=details.source_path) ELSE details.source_path END path
             FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value
             LEFT JOIN sources source ON source.source_key=track.source_key LEFT JOIN track_details details ON details.track_key=track.track_key) WHERE path<>''",
        )
        .bind(serde_json::to_string(&members)?)
        .fetch_one(&mut *connection)
        .await?;
        Ok(first.zip(last))
    }

    /// Tracks of an album, an artist or a single track URI, hidden ones included, by key, in
    /// music folder `folder`. An artist counts in one role, or in both when no role is given.
    async fn collection_members(
        &self,
        connection: &mut sqlx::SqliteConnection,
        uri: &str,
        album_artist: Option<bool>,
        folder: Option<crate::FolderKey>,
    ) -> LibraryResult<Vec<i64>> {
        let kind = crate::source_entity_parts(uri).map(|(_, kind, _)| kind);
        let owner: Option<i64> = match kind.as_deref() {
            Some("album") => sqlx::query_scalar("SELECT album_key FROM albums WHERE media_uri=?1"),
            Some("artist") => {
                sqlx::query_scalar("SELECT artist_key FROM artists WHERE media_uri=?1")
            }
            _ => sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1"),
        }
        .bind(uri)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(owner) = owner else {
            return Ok(Vec::new());
        };
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        let album = snapshot.albums.slot(owner);
        let artist = snapshot.artists.slot(owner);
        // A music folder narrows only its own source's artist.
        let folder = context
            .folder_slot(
                folder,
                artist.map(|slot| snapshot.artists.records[slot as usize].source),
            )
            .flatten();
        let mut members = snapshot
            .tracks
            .iter()
            .filter(|(_, track)| {
                let member = match kind.as_deref() {
                    Some("album") => Some(track.album) == album,
                    Some("artist") => artist.is_some_and(|artist| {
                        let credited = snapshot.track_runs(track)[0].contains(&artist);
                        let album_credited = context.album_artists(track).contains(&artist);
                        match album_artist {
                            Some(true) => album_credited,
                            Some(false) => credited,
                            None => credited || album_credited,
                        }
                    }),
                    _ => i64::from(track.key) == owner,
                };
                member
                    && folder.is_none_or(|folder| snapshot.track_runs(track)[3].contains(&folder))
            })
            .map(|(_, track)| i64::from(track.key))
            .collect::<Vec<_>>();
        members.sort_unstable();
        Ok(members)
    }

    pub async fn file_edit_rows(
        &self,
        collection_uri: &str,
        after: Option<crate::TrackKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FileEditRow>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let mut members = self
            .collection_members(&mut connection, collection_uri, None, None)
            .await?;
        members.retain(|key| after.is_none_or(|after| *key > after.raw()));
        members.truncate(128);
        Ok(sqlx::query_as("SELECT track.track_key,track.object_id,track.media_uri,CASE WHEN (SELECT kind FROM sources WHERE source_key=track.source_key)='local' THEN(SELECT file.path FROM files file WHERE file.source_key=track.source_key AND file.path=details.source_path) ELSE details.source_path END source_path,track.source_format,details.cue_path cue_path FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value LEFT JOIN track_details details USING(track_key) ORDER BY track.track_key")
            .bind(serde_json::to_string(&members)?).fetch_all(&mut *connection).await?)
    }

    pub async fn media_uris_for_document_ids(
        &self,
        ids: &[String],
    ) -> LibraryResult<Vec<Option<String>>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT (
                SELECT track.media_uri FROM sources source
                CROSS JOIN files file ON file.source_key=source.source_key AND file.native_id=requested.value
                CROSS JOIN track_details details ON details.source_path=file.path
                CROSS JOIN tracks track ON track.track_key=details.track_key AND track.source_key=file.source_key
                WHERE details.cue_path IS NULL AND details.cue_start_millis IS NULL
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
        let row:Option<(String,Option<String>,Option<String>)>=sqlx::query_as("SELECT track.object_id,details.source_path,CASE WHEN file.file_key IS NOT NULL THEN json_object('path',file.path,'root',file.root,'relative_path',file.relative_path,'kind',file.kind,'size_bytes',file.size_bytes,'mtime_ns',file.mtime_ns,'device_id',file.device_id,'inode',file.inode,'native_id',file.native_id,'revision',file.revision,'picture_index',file.picture_index,'artist_pictures',file.artist_pictures,'parse_version',file.parse_version,'state',file.state) END
            FROM tracks track LEFT JOIN track_details details USING(track_key)
            LEFT JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
            WHERE track.source_key=?1 AND track.media_uri=?2")
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

    /// Shows the current downloads in rows read earlier: a track's copy and how many tracks of
    /// a collection are downloaded. Smart playlists count their members as of `now`, the time
    /// the page read them at, in the music folder it read; None reads them as of now.
    pub async fn show_downloads(
        &self,
        rows: Vec<Shown<'_>>,
        (now, folder): (Option<i64>, Option<crate::FolderKey>),
        cancellation: &ReadCancellation,
    ) -> LibraryResult<()> {
        let smart = rows
            .iter()
            .filter_map(|row| match row {
                Shown::Smart(row) => Some(row.smart_playlist_key.raw()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let smart = if smart.is_empty() {
            self.loaded().await;
            std::collections::HashMap::new()
        } else {
            let mut connection = self.acquire_loaded(cancellation).await?;
            let now = now.unwrap_or_else(|| chrono::Utc::now().timestamp());
            crate::smart_playlists::smart_totals(self, &mut connection, &smart, now, folder).await?
        };
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        let downloaded = std::cell::OnceCell::new();
        let downloaded = || downloaded.get_or_init(|| self.downloaded(&context));
        let count =
            |counts: &[u32], slot: Option<u32>| slot.map(|slot| i64::from(counts[slot as usize]));
        for row in rows {
            let (shown, current) = match row {
                Shown::Track(row) => {
                    if let Some(downloaded) = snapshot.downloaded(row.track_key) {
                        row.is_downloaded = downloaded;
                    }
                    continue;
                }
                Shown::Album(row) => (
                    &mut row.summary.downloaded_count,
                    count(
                        &downloaded().albums,
                        snapshot.albums.slot(row.album_key.raw()),
                    ),
                ),
                Shown::Artist(row) => (
                    &mut row.summary.downloaded_count,
                    count(
                        &downloaded().artists[usize::from(row.album_artist)],
                        snapshot.artists.slot(row.artist_key.raw()),
                    ),
                ),
                Shown::Tag(row) => (
                    &mut row.summary.downloaded_count,
                    count(&downloaded().tags, snapshot.tags.slot(row.key)),
                ),
                Shown::Folder(row) => (
                    &mut row.summary.downloaded_count,
                    count(
                        &downloaded().folders,
                        row.folder_key
                            .and_then(|key| snapshot.folders.slot(key.raw())),
                    ),
                ),
                Shown::Playlist(row) => (
                    &mut row.downloaded_count,
                    snapshot
                        .playlists
                        .get(row.playlist_key.raw())
                        .map(|playlist| i64::from(context.playlist_totals(playlist).downloaded)),
                ),
                Shown::Smart(row) => (
                    &mut row.downloaded_count,
                    smart
                        .get(&row.smart_playlist_key.raw())
                        .map(|totals| i64::from(totals.downloaded)),
                ),
            };
            if let Some(current) = current {
                *shown = current;
            }
        }
        Ok(())
    }

    pub async fn downloaded_count(&self, source_id: Option<&str>) -> LibraryResult<usize> {
        let mut connection = self.acquire_cancellable(&ReadCancellation::new()).await?;
        let count = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(DISTINCT media_uri) FROM main.downloads WHERE source_id=?1",
            )
            .bind(source_id)
            .fetch_one(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(DISTINCT media_uri) FROM main.downloads WHERE source_id IS NULL",
            )
            .fetch_one(&mut *connection)
            .await
        };
        Ok(usize::try_from(count?).unwrap_or_default())
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

    pub async fn media_uri_for_file_path(
        &self,
        source: &str,
        path: &str,
    ) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM tracks track JOIN sources USING(source_key) JOIN track_details details USING(track_key) JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path WHERE sources.object_id=?1 AND details.source_path=?2 AND details.cue_path IS NULL LIMIT 1")
            .bind(source).bind(path).fetch_optional(&mut *connection).await?)
    }

    /// Listed albums and tracks of every source.
    pub async fn all_source_counts(
        &self,
    ) -> LibraryResult<std::collections::HashMap<crate::SourceId, (usize, usize)>> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        let derived = self.derived(&context, None);
        let mut counts = vec![(0usize, 0usize); snapshot.source_slots.len()];
        for (slot, album) in snapshot.albums.iter() {
            if derived.albums[slot as usize].tracks > 0
                && let Some(count) = counts.get_mut(usize::from(album.source))
            {
                count.0 += 1;
            }
        }
        for (_, track) in snapshot.tracks.iter() {
            if track.indexed()
                && let Some(count) = counts.get_mut(usize::from(track.source))
            {
                count.1 += 1;
            }
        }
        Ok(snapshot
            .sources
            .values()
            .filter_map(|source| {
                let slot = snapshot.source_slot(source.key)?;
                Some((
                    crate::SourceId::new(source.object.clone()),
                    counts[usize::from(slot)],
                ))
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
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks JOIN track_details USING(track_key)
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

    pub async fn mapping_track_page(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        source_path: Option<&str>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MappingTrackRow>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        Ok(sqlx::query_as::<_, MappingTrackRow>(
            "SELECT track_key,media_uri,source_path
             FROM tracks JOIN track_details USING(track_key) WHERE source_key=?1 AND indexed=1 AND source_path IS NOT NULL
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

impl Catalog {
    pub async fn upsert_local_locator(
        &self,
        locator: &LocalLocatorWrite,
    ) -> LibraryResult<LocalAccessFileKey> {
        let mut write = self.write().await?;
        let key = write_local_locator(&mut write, locator).await?;
        write.commit().await?;
        Ok(key)
    }
}

pub(crate) async fn write_local_locator(
    write: &mut Write<'_>,
    locator: &LocalLocatorWrite,
) -> LibraryResult<LocalAccessFileKey> {
    write
        .put_reference(&crate::TrackReference {
            media_uri: locator.media_uri.clone(),
            ..Default::default()
        })
        .await?;
    let source = match &locator.source_id {
        Some(id) => Some(write.source_identity(id).await?),
        None => None,
    };
    let key = sqlx::query_scalar("INSERT INTO main.local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(media_uri,origin) DO UPDATE SET source_key=excluded.source_key,path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING local_access_file_key")
        .bind(source).bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(write.connection().await?).await?;
    write.keep();
    write.read_downloads(&[&locator.media_uri]).await?;
    Ok(key)
}

pub(crate) async fn export_local_locators_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, LocalLocatorWrite>("SELECT source.object_id source_id,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM main.local_locators locator LEFT JOIN main.sources source USING(source_key) ORDER BY locator.local_access_file_key").fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}
/// Restores imported and mapped locators. Downloads are not part of a backup; lines of
/// released backups that name a download are skipped.
pub(crate) async fn import_local_locators_jsonl_on(
    write: &mut Write<'_>,
    input: impl std::io::BufRead,
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
        // Downloads and folder mappings are not locators any more.
        if matches!(locator.origin.as_str(), "download" | "mapping") {
            continue;
        }
        write_local_locator(write, &locator).await?;
        count += 1;
    }
    Ok(count)
}

impl Catalog {
    /// A playlist's local entries by URI, with the path a walk reads each from: the CUE sheet
    /// or file its track was read from, else the import locator's path.
    pub async fn playlist_import_page(
        &self,
        playlist: crate::PlaylistKey,
        after: &str,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let mut connection = self.acquire_cancellable(&ReadCancellation::new()).await?;
        Ok(sqlx::query_as(
            "SELECT DISTINCT track.media_uri,coalesce(details.cue_path,details.source_path,(SELECT locator.path FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import'))
             FROM playlist_entries entry JOIN tracks track USING(track_key) LEFT JOIN track_details details USING(track_key)
             WHERE entry.playlist_key=?1 AND track.media_uri>?2
               AND(track.media_uri LIKE 'file:%' OR track.media_uri LIKE 'rufin:cue/%' OR track.media_uri LIKE 'rufin:document/%' OR track.media_uri LIKE 'content:%')
             ORDER BY track.media_uri LIMIT 128",
        )
        .bind(playlist)
        .bind(after)
        .fetch_all(&mut *connection)
        .await?)
    }
}
