use crate::{Catalog, LibraryResult, LocalArtworkCandidate};

/// Tracks of a directory or the directory below it: `prefix` ends with the separator.
const TWO_LEVELS: &str = "(instr(substr(details.source_path,length(scope.prefix)+1),substr(scope.prefix,-1))=0 OR instr(substr(details.source_path,length(scope.prefix)+1+instr(substr(details.source_path,length(scope.prefix)+1),substr(scope.prefix,-1))),substr(scope.prefix,-1))=0)";

impl Catalog {
    pub async fn equivalent_local_file_uris(
        &self,
        files: &[(String, String, Option<i64>, Option<i64>)],
    ) -> LibraryResult<Vec<(String, String)>> {
        let mut connection = self.acquire_reader().await?;
        let sources: Vec<i64> = sqlx::query_scalar(
            "SELECT source_key FROM sources WHERE kind='local' ORDER BY source_key",
        )
        .fetch_all(&mut *connection)
        .await?;
        let mut resolved = std::collections::BTreeMap::new();
        for source in sources {
            for (previous, current, _) in
                equivalent_cue_uris_on(&mut connection, source, files).await?
            {
                resolved.entry(previous).or_insert(current);
            }
        }
        Ok(resolved.into_iter().collect())
    }

    /// Albums with an indexed track in one of `directories` or the directory below it, or
    /// every album with an indexed file track when `directories` is None, in one statement
    /// so the scope is matched once.
    pub async fn source_artwork_albums(
        &self,
        source_id: &str,
        directories: Option<&[String]>,
    ) -> LibraryResult<Vec<String>> {
        let held = match directories {
            None => "EXISTS(SELECT 1 FROM tracks track JOIN track_details details USING(track_key) WHERE track.album_key=album.album_key AND track.indexed AND details.source_path IS NOT NULL)".to_owned(),
            Some(_) => format!("album.album_key IN(SELECT track.album_key FROM scope CROSS JOIN track_details details ON details.source_path>=scope.prefix AND details.source_path<scope.prefix||char(1114111) CROSS JOIN tracks track USING(track_key) WHERE track.indexed AND {TWO_LEVELS})"),
        };
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH scope(prefix) AS(SELECT value FROM json_each(?2)) SELECT album.object_id FROM albums album JOIN sources source USING(source_key) WHERE source.object_id=?1 AND {held}")))
            .bind(source_id).bind(directories.map(serde_json::to_string).transpose()?).fetch_all(&mut *connection).await?)
    }

    /// Artists credited by an indexed track under one of `directories`, whose portraits can
    /// sit in any folder above the track, or every such artist when `directories` is None.
    pub async fn source_artwork_artists(
        &self,
        source_id: &str,
        directories: Option<&[String]>,
    ) -> LibraryResult<Vec<(String, String)>> {
        let held = match directories {
            None => {
                "EXISTS(SELECT 1 FROM track_links credit JOIN tracks track USING(track_key) JOIN track_details details USING(track_key) WHERE credit.role IN(0,1) AND credit.target=artist.artist_key AND track.indexed AND details.source_path IS NOT NULL)"
            }
            Some(_) => {
                "artist.artist_key IN(SELECT credit.target FROM scope CROSS JOIN track_details details ON details.source_path>=scope.prefix AND details.source_path<scope.prefix||char(1114111) CROSS JOIN tracks track USING(track_key) CROSS JOIN track_links credit ON credit.track_key=track.track_key AND credit.role IN(0,1) WHERE track.indexed)"
            }
        };
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("WITH scope(prefix) AS(SELECT value FROM json_each(?2)) SELECT artist.object_id,artist.name FROM artists artist JOIN sources source USING(source_key) WHERE source.object_id=?1 AND {held}")))
            .bind(source_id).bind(directories.map(serde_json::to_string).transpose()?).fetch_all(&mut *connection).await?)
    }

    pub async fn file_artwork_image_page(
        &self,
        source_id: &str,
        prefix: &str,
        after: &str,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT file.path,file.revision FROM files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.kind='image' AND file.path>=?2 AND file.path<?2||char(1114111) AND file.path>?3 AND instr(substr(file.path,length(?2)+1),'/')=0 ORDER BY file.path LIMIT 128")
            .bind(source_id).bind(prefix).bind(after).fetch_all(&mut *connection).await?)
    }

    /// The indexed file tracks of `albums`, by album, in album order.
    pub async fn source_artwork_album_tracks(
        &self,
        source_id: &str,
        albums: &[String],
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT album.object_id owner,track.object_id,file.path,track.disc_number,track.track_number,track.sort_text,0 primary_artist,file.root,file.picture_index,file.artist_pictures,coalesce(file.revision,CAST(file.size_bytes AS TEXT)||'-'||CAST(file.mtime_ns AS TEXT)) revision
            FROM json_each(?2) requested CROSS JOIN albums album ON album.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND album.object_id=requested.value
            CROSS JOIN tracks track ON track.album_key=album.album_key CROSS JOIN track_details details ON details.track_key=track.track_key CROSS JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
            WHERE track.indexed ORDER BY album.object_id,track.disc_number,track.track_number,track.sort_text,track.object_id")
            .bind(source_id).bind(serde_json::to_string(albums)?).fetch_all(&mut *connection).await?)
    }

    /// The indexed file tracks crediting `artists`, by artist, in object id order; a track
    /// whose first credited artist it is is primary.
    pub async fn source_artwork_artist_tracks(
        &self,
        source_id: &str,
        artists: &[&str],
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT artist.object_id owner,track.object_id,file.path,track.disc_number,track.track_number,track.sort_text,coalesce((SELECT first.object_id FROM track_links credit JOIN artists first ON first.artist_key=credit.target WHERE credit.track_key=track.track_key AND credit.role=1 ORDER BY credit.position LIMIT 1)=artist.object_id,0) primary_artist,file.root,file.picture_index,file.artist_pictures,coalesce(file.revision,CAST(file.size_bytes AS TEXT)||'-'||CAST(file.mtime_ns AS TEXT)) revision
            FROM json_each(?2) requested CROSS JOIN artists artist ON artist.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND artist.object_id=requested.value
            CROSS JOIN tracks track ON track.track_key IN(SELECT credit.track_key FROM track_links credit WHERE credit.role IN(0,1) AND credit.target=artist.artist_key)
            CROSS JOIN track_details details ON details.track_key=track.track_key CROSS JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
            WHERE track.indexed ORDER BY artist.object_id,track.object_id")
            .bind(source_id).bind(serde_json::to_string(artists)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_directory_is_single_album(
        &self,
        source_id: &str,
        prefix: &str,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        let count: i64 = sqlx::query_scalar("SELECT count(DISTINCT track.album_key) FROM track_details details CROSS JOIN tracks track USING(track_key) WHERE details.source_path>=?2 AND details.source_path<?2||char(1114111) AND track.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND track.indexed")
            .bind(source_id).bind(prefix).fetch_one(&mut *connection).await?;
        Ok(count == 1)
    }

    pub async fn source_artwork_directory_has_artist(
        &self,
        source_id: &str,
        prefix: &str,
        artist: &str,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("WITH directory AS(SELECT track.track_key FROM track_details details CROSS JOIN tracks track USING(track_key) WHERE details.source_path>=?2 AND details.source_path<?2||char(1114111) AND track.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND track.indexed) SELECT EXISTS(SELECT 1 FROM directory) AND NOT EXISTS(SELECT 1 FROM directory WHERE NOT EXISTS(SELECT 1 FROM track_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.track_key=directory.track_key AND credit.role=1 AND artist.object_id=?3 AND credit.position=(SELECT min(position) FROM track_links WHERE track_key=directory.track_key AND role=1)))")
            .bind(source_id).bind(prefix).bind(artist).fetch_one(&mut *connection).await?)
    }
}

impl super::Observation<'_> {
    pub async fn clear_partial_file_image_observations(&mut self) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let cleared = sqlx::query("UPDATE files SET artist_pictures=NULL WHERE source_key=?1 AND kind='media' AND picture_index IS NULL AND artist_pictures IS NOT NULL AND path NOT LIKE 'rufin-document://%' AND path NOT LIKE 'content://%'")
            .bind(self.source).execute(write.connection().await?).await?.rows_affected();
        keep_file_rows(write, cleared).await
    }
}

/// File rows are not resident and no client shows them, so a Write that changed some keeps
/// them without publishing.
async fn keep_file_rows(mut write: super::write::Write<'_>, changed: u64) -> LibraryResult<()> {
    if changed != 0 {
        write.keep();
    }
    write.commit().await?;
    Ok(())
}

// A sole zero-start CUE entry plays its backing file through EOF.
pub(super) async fn equivalent_cue_uris_on(
    connection: &mut sqlx::SqliteConnection,
    source: i64,
    files: &[(String, String, Option<i64>, Option<i64>)],
) -> LibraryResult<Vec<(String, String, String)>> {
    let mut aliases = Vec::new();
    let whole_file = crate::local::whole_file_cue_sql("candidate", "candidate_details");
    for page in files.chunks(128) {
        aliases.extend(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT json_extract(input.value,'$[0]'),cue.media_uri,cue.object_id
            FROM json_each(?2) input CROSS JOIN tracks cue ON cue.track_key=(
                WITH paths(path) AS MATERIALIZED(SELECT json_extract(input.value,'$[1]') UNION SELECT known_details.source_path FROM tracks known JOIN track_details known_details USING(track_key) WHERE known.source_key=?1 AND known.media_uri=json_extract(input.value,'$[0]') AND known_details.cue_path IS NULL AND known_details.source_path IS NOT NULL UNION SELECT path FROM files WHERE source_key=?1 AND device_id=json_extract(input.value,'$[2]') AND inode=json_extract(input.value,'$[3]'))
                SELECT candidate.track_key FROM paths CROSS JOIN track_details candidate_details INDEXED BY track_details_path_idx ON candidate_details.source_path=paths.path CROSS JOIN tracks candidate ON candidate.track_key=candidate_details.track_key WHERE candidate.source_key=?1
                  AND {whole_file} AND EXISTS(SELECT 1 FROM files file WHERE file.source_key=candidate.source_key AND file.path=candidate_details.cue_path)
                ORDER BY candidate_details.cue_path,candidate.object_id LIMIT 1)
            WHERE cue.media_uri<>json_extract(input.value,'$[0]')")))
            .bind(source).bind(serde_json::to_string(page)?).fetch_all(&mut *connection).await?);
    }
    Ok(aliases)
}

pub(super) async fn referenced_file_page_on(
    connection: &mut sqlx::SqliteConnection,
    source: i64,
    paths: &[String],
    after: i64,
) -> LibraryResult<Vec<(i64, String, String)>> {
    Ok(sqlx::query_as("SELECT tracks.track_key,tracks.media_uri,location.source_path FROM json_each(?2) requested CROSS JOIN track_details location ON location.source_path=requested.value CROSS JOIN tracks USING(track_key)
        WHERE tracks.source_key=?1 AND tracks.track_key>?3 AND location.cue_path IS NULL AND media_uri NOT LIKE 'rufin:cue/%'
          AND(object_id IS NOT NULL OR indexed=1
            OR EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM queue_entries entry WHERE entry.track_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM home_entries entry WHERE entry.entity_kind='track' AND entry.entity_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM user_state state WHERE state.media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM listens WHERE media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM lyrics_cache WHERE media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM outbox WHERE media_uri=tracks.media_uri)
            OR tracks.native_favorite IS NOT NULL OR tracks.native_rating IS NOT NULL OR tracks.native_play_count IS NOT NULL)
        ORDER BY tracks.track_key LIMIT 128")
        .bind(source).bind(serde_json::to_string(paths)?).bind(after).fetch_all(connection).await?)
}
