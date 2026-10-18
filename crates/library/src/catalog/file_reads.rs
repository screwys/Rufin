use crate::{Database, LibraryResult, LocalArtworkCandidate, LocalFileKind, LocalFileWrite};

const FILE_FIELDS: &str = "file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.native_id,file.revision,file.picture_index,file.artist_pictures,file.parse_version,file.state";

impl Database {
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

    pub async fn source_file_reads_complete(
        &self,
        source_id: &str,
        stamp: i64,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.observed_stamp=?2 AND file.state='unreadable')").bind(source_id).bind(stamp).fetch_one(&mut *connection).await?)
    }

    pub async fn source_files(
        &self,
        source_id: &str,
        paths: &[String],
    ) -> LibraryResult<Vec<LocalFileWrite>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {FILE_FIELDS} FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.path IN(SELECT value FROM json_each(?2))")))
            .bind(source_id).bind(serde_json::to_string(paths)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_file_page(
        &self,
        source_id: &str,
        kind: LocalFileKind,
        stamp: Option<i64>,
        after: Option<&str>,
        parent: Option<&str>,
        root: Option<&str>,
    ) -> LibraryResult<Vec<LocalFileWrite>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {FILE_FIELDS} FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.kind=?2 AND file.path>coalesce(?3,'') AND(?4 IS NULL OR file.observed_stamp=?4) AND(?5 IS NULL OR file.parent_path=?5) AND(?6 IS NULL OR file.root=?6) ORDER BY file.path LIMIT 128")))
            .bind(source_id).bind(kind).bind(after).bind(stamp).bind(parent.map(super::files::directory_locator)).bind(root).fetch_all(&mut *connection).await?)
    }

    pub async fn source_next_directory(
        &self,
        source_id: &str,
        stamp: Option<i64>,
        after: Option<&str>,
        root: &str,
    ) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT file.path FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.kind='directory' AND file.root=?2 AND file.path>coalesce(?3,'') AND(?4 IS NULL OR file.observed_stamp=?4) ORDER BY file.path LIMIT 1")
            .bind(source_id).bind(root).bind(after).bind(stamp).fetch_optional(&mut *connection).await?)
    }

    pub async fn source_file_native_paths(
        &self,
        source_id: &str,
        native_id: &str,
        observed: Option<i64>,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT file.path FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.native_id=?2 AND(?3 IS NULL OR file.observed_stamp=?3) ORDER BY file.path")
            .bind(source_id).bind(native_id).bind(observed).fetch_all(&mut *connection).await?)
    }

    pub async fn source_file_track_identity(
        &self,
        source_id: &str,
        path: &str,
    ) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.object_id FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1 AND coalesce(track.cue_path,track.source_path)=?2 AND track.object_id IS NOT NULL ORDER BY track.track_key LIMIT 1").bind(source_id).bind(path).fetch_optional(&mut *connection).await?)
    }

    pub async fn source_track_file(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<(String, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT object_id,source_path FROM tracks WHERE media_uri=?1 AND source_path IS NOT NULL AND object_id IS NOT NULL")
            .bind(media_uri).fetch_optional(&mut *connection).await?)
    }

    pub async fn source_cue_dependencies_unchanged(
        &self,
        source_id: &str,
        cue_path: &str,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM local_files cue JOIN sources source USING(source_key) WHERE source.object_id=?1 AND cue.path=?2 AND cue.state='accepted' AND EXISTS(SELECT 1 FROM local_file_dependencies dependency WHERE dependency.local_file_key=cue.local_file_key) AND NOT EXISTS(SELECT 1 FROM local_file_dependencies dependency LEFT JOIN local_files backing ON backing.source_key=cue.source_key AND backing.path=dependency.dependency_path WHERE dependency.local_file_key=cue.local_file_key AND(backing.path IS NULL OR backing.state<>'accepted' OR(backing.revision IS NULL AND backing.mtime_ns=0) OR dependency.accepted_inputs IS NOT json_array(backing.size_bytes,backing.mtime_ns,backing.revision))))")
            .bind(source_id).bind(cue_path).fetch_one(&mut *connection).await?)
    }

    pub async fn source_dependency_paths(
        &self,
        source_id: &str,
        paths: &[String],
        observed: Option<i64>,
        admitted_only: bool,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT DISTINCT dependency.dependency_path FROM local_file_dependencies dependency JOIN local_files file USING(local_file_key) JOIN sources source USING(source_key) WHERE source.object_id=?1 AND dependency.dependency_path IN(SELECT value FROM json_each(?2)) AND(?3 IS NULL OR file.observed_stamp=?3) AND(?4=0 OR EXISTS(SELECT 1 FROM tracks segment WHERE segment.local_file_key=file.local_file_key AND segment.cue_path=file.path AND segment.indexed=1))")
            .bind(source_id).bind(serde_json::to_string(paths)?).bind(observed).bind(admitted_only).fetch_all(&mut *connection).await?)
    }

    pub async fn source_component_page(
        &self,
        source_id: &str,
        seeds: &[String],
        prefixes: &[String],
        after: Option<&str>,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("WITH RECURSIVE component(path) AS(SELECT value FROM json_each(?2) UNION SELECT file.path FROM local_files file JOIN sources source USING(source_key) JOIN json_each(?3) prefix ON file.path>=prefix.value AND file.path<prefix.value||char(1114111) WHERE source.object_id=?1 UNION SELECT dependency.dependency_path FROM component JOIN local_files file ON file.path=component.path JOIN sources source USING(source_key) JOIN local_file_dependencies dependency USING(local_file_key) WHERE source.object_id=?1 UNION SELECT file.path FROM component JOIN local_file_dependencies dependency ON dependency.dependency_path=component.path JOIN local_files file USING(local_file_key) JOIN sources source USING(source_key) WHERE source.object_id=?1) SELECT path FROM component WHERE path>coalesce(?4,'') ORDER BY path LIMIT 128")
            .bind(source_id).bind(serde_json::to_string(seeds)?).bind(serde_json::to_string(prefixes)?).bind(after).fetch_all(&mut *connection).await?)
    }

    pub async fn source_observed_media_uris(
        &self,
        source_id: &str,
        stamp: i64,
        uris: &[&str],
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1 AND track.observed_stamp=?2 AND track.media_uri IN(SELECT value FROM json_each(?3)) ORDER BY track.track_key")
            .bind(source_id).bind(stamp).bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_count(
        &self,
        source_id: &str,
        stamp: Option<i64>,
    ) -> LibraryResult<usize> {
        let mut connection = self.acquire_reader().await?;
        let count: i64 = sqlx::query_scalar("WITH eligible AS(SELECT track.track_key,track.album_key FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1 AND track.indexed AND(?2 IS NULL OR track.observed_stamp=?2 OR EXISTS(SELECT 1 FROM local_files file WHERE file.source_key=track.source_key AND file.path=coalesce(track.cue_path,track.source_path) AND file.observed_stamp=?2))) SELECT(SELECT count(DISTINCT album_key) FROM eligible)+(SELECT count(*) FROM(SELECT credit.artist_key FROM track_artists credit JOIN eligible USING(track_key) UNION SELECT credit.artist_key FROM track_album_artists credit JOIN eligible USING(track_key)))")
            .bind(source_id).bind(stamp).fetch_one(&mut *connection).await?;
        Ok(count as usize)
    }

    pub async fn source_artwork_album_page(
        &self,
        source_id: &str,
        after: &str,
        stamp: Option<i64>,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT album.object_id FROM albums album JOIN sources source USING(source_key) WHERE source.object_id=?1 AND album.object_id>?2 AND EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=album.album_key AND tracks.indexed AND(?3 IS NULL OR tracks.observed_stamp=?3 OR EXISTS(SELECT 1 FROM local_files file WHERE file.source_key=tracks.source_key AND file.path=coalesce(tracks.cue_path,tracks.source_path) AND file.observed_stamp=?3))) ORDER BY album.object_id LIMIT 128")
            .bind(source_id).bind(after).bind(stamp).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_artist_page(
        &self,
        source_id: &str,
        after: &str,
        stamp: Option<i64>,
    ) -> LibraryResult<Vec<(String, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT artist.object_id,artist.name FROM artists artist JOIN sources source USING(source_key) WHERE source.object_id=?1 AND artist.object_id>?2 AND EXISTS(SELECT 1 FROM track_artists credit JOIN tracks track USING(track_key) WHERE credit.artist_key=artist.artist_key AND track.indexed AND(?3 IS NULL OR track.observed_stamp=?3 OR EXISTS(SELECT 1 FROM local_files file WHERE file.source_key=track.source_key AND file.path=coalesce(track.cue_path,track.source_path) AND file.observed_stamp=?3)) UNION SELECT 1 FROM track_album_artists credit JOIN tracks track USING(track_key) WHERE credit.artist_key=artist.artist_key AND track.indexed AND(?3 IS NULL OR track.observed_stamp=?3 OR EXISTS(SELECT 1 FROM local_files file WHERE file.source_key=track.source_key AND file.path=coalesce(track.cue_path,track.source_path) AND file.observed_stamp=?3))) ORDER BY artist.object_id LIMIT 128")
            .bind(source_id).bind(after).bind(stamp).fetch_all(&mut *connection).await?)
    }

    pub async fn file_artwork_image_page(
        &self,
        source_id: &str,
        prefix: &str,
        after: &str,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT file.path,file.revision FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.kind='image' AND file.path>=?2 AND file.path<?2||char(1114111) AND file.path>?3 AND instr(substr(file.path,length(?2)+1),'/')=0 ORDER BY file.path LIMIT 128")
            .bind(source_id).bind(prefix).bind(after).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_track_page(
        &self,
        source_id: &str,
        album: &str,
        after: Option<&LocalArtworkCandidate>,
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT track.object_id,track.source_path path,track.disc_number,track.track_number,track.sort_text,0 primary_artist,file.root,file.picture_index,file.artist_pictures,coalesce(file.revision,CAST(file.size_bytes AS TEXT)||'-'||CAST(file.mtime_ns AS TEXT)) revision FROM tracks track JOIN albums album USING(album_key) JOIN sources source ON source.source_key=album.source_key LEFT JOIN local_files file ON file.source_key=track.source_key AND file.path=track.source_path WHERE source.object_id=?1 AND album.object_id=?2 AND track.indexed AND track.source_path IS NOT NULL AND(?3 IS NULL OR(track.disc_number,track.track_number,track.sort_text,track.object_id)>(?4,?5,?6,?3)) ORDER BY track.disc_number,track.track_number,track.sort_text,track.object_id LIMIT 128")
            .bind(source_id).bind(album).bind(after.map(|row|&row.object_id)).bind(after.map(|row|row.disc_number)).bind(after.map(|row|row.track_number)).bind(after.map(|row|&row.sort_text)).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_artist_track_page(
        &self,
        source_id: &str,
        artist: &str,
        after: &str,
    ) -> LibraryResult<Vec<LocalArtworkCandidate>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT track.object_id,track.source_path path,track.disc_number,track.track_number,track.sort_text,coalesce((SELECT artist.object_id FROM track_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1)=?2,0) primary_artist,file.root,file.picture_index,file.artist_pictures,coalesce(file.revision,CAST(file.size_bytes AS TEXT)||'-'||CAST(file.mtime_ns AS TEXT)) revision FROM tracks track JOIN sources source USING(source_key) LEFT JOIN local_files file ON file.source_key=track.source_key AND file.path=track.source_path WHERE source.object_id=?1 AND track.indexed AND track.object_id>?3 AND track.source_path IS NOT NULL AND track.track_key IN(SELECT credit.track_key FROM artists artist JOIN track_artists credit USING(artist_key) WHERE artist.source_key=track.source_key AND artist.object_id=?2 UNION SELECT credit.track_key FROM artists artist JOIN track_album_artists credit USING(artist_key) WHERE artist.source_key=track.source_key AND artist.object_id=?2) ORDER BY track.object_id LIMIT 128")
            .bind(source_id).bind(artist).bind(after).fetch_all(&mut *connection).await?)
    }

    pub async fn source_artwork_directory_is_single_album(
        &self,
        source_id: &str,
        prefix: &str,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        let count: i64 = sqlx::query_scalar("SELECT count(DISTINCT album_key) FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1 AND track.indexed AND track.source_path>=?2 AND track.source_path<?2||char(1114111)")
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
        Ok(sqlx::query_scalar("WITH directory AS(SELECT track.track_key FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1 AND track.indexed AND track.source_path>=?2 AND track.source_path<?2||char(1114111)) SELECT EXISTS(SELECT 1 FROM directory) AND NOT EXISTS(SELECT 1 FROM directory WHERE NOT EXISTS(SELECT 1 FROM track_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=directory.track_key AND artist.object_id=?3 AND credit.position=(SELECT min(position) FROM track_album_artists WHERE track_key=directory.track_key)))")
            .bind(source_id).bind(prefix).bind(artist).fetch_one(&mut *connection).await?)
    }
}

impl super::Catalog<'_> {
    pub async fn clear_partial_file_image_observations(&mut self) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        sqlx::query("UPDATE local_files SET artist_pictures=NULL WHERE source_key=?1 AND kind='media' AND picture_index IS NULL AND artist_pictures IS NOT NULL AND path NOT LIKE 'rufin-document://%' AND path NOT LIKE 'content://%'")
            .bind(self.source).execute(connection).await?;
        Ok(())
    }

    pub async fn record_source_file_images(
        &mut self,
        rows: &[(String, Option<i64>, String)],
    ) -> LibraryResult<()> {
        let source_id = self.source_id;

        if rows.is_empty() {
            return Ok(());
        }
        let mut writer = self.database.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        sqlx::query("UPDATE local_files SET picture_index=json_extract(item.value,'$[1]'),artist_pictures=json_extract(item.value,'$[2]') FROM json_each(?2) item WHERE local_files.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND local_files.path=json_extract(item.value,'$[0]') AND(local_files.picture_index,local_files.artist_pictures) IS NOT(json_extract(item.value,'$[1]'),json_extract(item.value,'$[2]'))").bind(source_id).bind(serde_json::to_string(rows)?).execute(connection).await?;
        Ok(())
    }
    pub async fn observe_cached_file_tree(&mut self, file: &LocalFileWrite) -> LibraryResult<bool> {
        let source_id = self.source_id;
        let stamp = self.observed;

        let mut writer = self.database.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        let parsed = serde_json::to_string(&(
            file.size_bytes,
            file.mtime_ns,
            &file.revision,
            file.parse_version,
        ))?;
        let reusable:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=?1 AND file.path=?2 AND file.state='accepted' AND json_remove(file.parsed_stamp,'$[4]')=?3)")
            .bind(source_id).bind(&file.path).bind(parsed).fetch_one(&mut *connection).await?;
        if reusable {
            sqlx::query("WITH RECURSIVE accepted(local_file_key,path,listed) AS(SELECT local_file_key,path,coalesce(json_extract(parsed_stamp,'$[4]'),observed_stamp) FROM local_files WHERE source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND path=?2 UNION ALL SELECT child.local_file_key,child.path,coalesce(json_extract(child.parsed_stamp,'$[4]'),child.observed_stamp) FROM accepted parent JOIN local_files child ON child.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND child.parent_path=rtrim(parent.path,'/'||char(92)) AND child.observed_stamp>=parent.listed AND child.path<>parent.path) UPDATE local_files SET parsed_stamp=CASE WHEN kind='directory' AND parsed_stamp IS NOT NULL THEN json_set(parsed_stamp,'$[4]',coalesce(?3,observed_stamp)) ELSE parsed_stamp END,observed_stamp=coalesce(?3,observed_stamp) WHERE local_file_key IN(SELECT local_file_key FROM accepted)")
                .bind(source_id).bind(&file.path).bind(stamp).execute(connection).await?;
        }
        Ok(reusable)
    }
    pub async fn observe_file_children(&mut self, path: &str) -> LibraryResult<()> {
        let source_id = self.source_id;
        let stamp = self.observed;

        let mut writer = self.database.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        sqlx::query("UPDATE local_files SET observed_stamp=coalesce(?3,observed_stamp) WHERE source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND parent_path=?2 AND observed_stamp>=(SELECT coalesce(json_extract(parsed_stamp,'$[4]'),observed_stamp) FROM local_files parent WHERE parent.source_key=local_files.source_key AND rtrim(parent.path,'/'||char(92))=?2 AND parent.kind='directory' LIMIT 1)")
            .bind(source_id).bind(super::files::directory_locator(path)).bind(stamp).execute(connection).await?;
        Ok(())
    }
}

// A sole zero-start CUE entry plays its backing file through EOF.
pub(super) async fn equivalent_cue_uris_on(
    connection: &mut sqlx::SqliteConnection,
    source: i64,
    files: &[(String, String, Option<i64>, Option<i64>)],
) -> LibraryResult<Vec<(String, String, String)>> {
    let mut aliases = Vec::new();
    let whole_file = crate::local::whole_file_cue_sql("candidate");
    for page in files.chunks(128) {
        aliases.extend(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT json_extract(input.value,'$[0]'),cue.media_uri,cue.object_id
            FROM json_each(?2) input CROSS JOIN tracks cue ON cue.track_key=(
                SELECT candidate.track_key FROM tracks candidate INDEXED BY tracks_source_path_idx WHERE candidate.source_key=?1 AND candidate.source_path IN(SELECT json_extract(input.value,'$[1]') UNION SELECT source_path FROM tracks known WHERE known.source_key=?1 AND known.media_uri=json_extract(input.value,'$[0]') AND known.cue_path IS NULL UNION SELECT path FROM local_files WHERE source_key=?1 AND device_id=json_extract(input.value,'$[2]') AND inode=json_extract(input.value,'$[3]'))
                  AND {whole_file}
                ORDER BY candidate.cue_path,candidate.object_id LIMIT 1)
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
    Ok(sqlx::query_as("SELECT track_key,media_uri,source_path FROM tracks
        WHERE source_key=?1 AND source_path IN(SELECT value FROM json_each(?2))
          AND track_key>?3 AND cue_path IS NULL AND media_uri NOT LIKE 'rufin:cue/%'
          AND(object_id IS NOT NULL OR indexed=1 OR local_file_key IS NOT NULL
            OR EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM queue_entries entry WHERE entry.track_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM home_entries entry WHERE entry.entity_kind='track' AND entry.entity_key=tracks.track_key)
            OR EXISTS(SELECT 1 FROM user_media_state state WHERE state.media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM listens WHERE media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM lyrics_cache WHERE media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM favorite_outbox WHERE media_uri=tracks.media_uri)
            OR EXISTS(SELECT 1 FROM native_media_state WHERE media_uri=tracks.media_uri))
        ORDER BY track_key LIMIT 128")
        .bind(source).bind(serde_json::to_string(paths)?).bind(after).fetch_all(connection).await?)
}
