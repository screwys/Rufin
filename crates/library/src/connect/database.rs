//! Whole-Store profile copies for joining a profile.
use super::*;
use sqlx::Connection;

impl Catalog {
    pub(crate) async fn connect_prepare_export_on(
        connection: &mut SqliteConnection,
    ) -> LibraryResult<()> {
        sqlx::raw_sql("DELETE FROM queue_state; DELETE FROM queue_entries; DELETE FROM queue_contexts; DELETE FROM queue_transfer_pages;
            DELETE FROM outbox; DELETE FROM listen_outbox; DELETE FROM local_locators; DELETE FROM downloads;
            DELETE FROM playlist_file_links; DELETE FROM playlist_file_ignored; DELETE FROM playlist_file_settings;
            DELETE FROM files WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
            UPDATE connect_profile SET enabled=0;")
            .execute(connection).await?;
        Ok(())
    }

    /// Downloaded Connect media the incoming profile may not know, with the local rows that
    /// follow each track.
    pub(crate) async fn downloaded_references_on(
        connection: &mut SqliteConnection,
    ) -> LibraryResult<Vec<Value>> {
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT json_object('media_uri',track.media_uri,'metadata',json({}),'artwork',CASE WHEN details.native_artwork IS NULL THEN NULL ELSE hex(details.native_artwork) END,'source_path',details.source_path,'cue_path',details.cue_path,'cue_start_millis',details.cue_start_millis,'cue_end_millis',details.cue_end_millis,
            'locators',(SELECT json_group_array(json_object('source_id',source.object_id,'origin',locator.origin,'path',locator.path,'root',locator.root,'relative_path',locator.relative_path,'access_uri',locator.access_uri)) FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE locator.media_uri=track.media_uri))
            FROM tracks track LEFT JOIN track_details details USING(track_key) WHERE EXISTS(SELECT 1 FROM downloads file WHERE file.media_uri=track.media_uri) ORDER BY track.media_uri", crate::track_metadata_sql("track", "details"))))
            .fetch_all(connection).await?;
        rows.iter()
            .map(|row| Ok(serde_json::from_str(row)?))
            .collect()
    }

    /// Puts the downloaded references the adopted Store lacks back as they were.
    pub(crate) async fn keep_downloaded_references(
        write: &mut crate::catalog::write::Write<'_>,
        kept: &[Value],
    ) -> LibraryResult<()> {
        use crate::catalog::records::Track;
        for reference in kept {
            let uri = reference["media_uri"].as_str().unwrap_or_default();
            if write.by_natural::<Track>(uri).await?.is_some() {
                continue;
            }
            let key = write
                .put_reference(&crate::TrackReference {
                    media_uri: uri.to_owned(),
                    metadata: serde_json::from_value(reference["metadata"].clone())?,
                    artwork_binding: hex_bytes(&reference["artwork"]),
                })
                .await?;
            // A known track with the same source identity keeps its own facts.
            let Some(mut track) = write
                .get::<Track>(key.raw())
                .await?
                .filter(|track| track.uri == uri)
            else {
                continue;
            };
            track.source_path = reference["source_path"].as_str().map(str::to_owned);
            track.cue_path = reference["cue_path"].as_str().map(str::to_owned);
            track.cue_start = reference["cue_start_millis"].as_i64();
            track.cue_end = reference["cue_end_millis"].as_i64();
            track.writes.metadata = Some(crate::metadata::track_details_value(
                reference["metadata"].clone(),
            ));
            write.put(track);
            write.keep();
            write.read_downloads(&[uri]).await?;
            let connection = write.connection().await?;
            sqlx::query("INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) SELECT (SELECT source_key FROM sources WHERE object_id=locator.value->>'$.source_id'),?1,locator.value->>'$.origin',locator.value->>'$.path',locator.value->>'$.root',locator.value->>'$.relative_path',locator.value->>'$.access_uri' FROM json_each(?2) locator")
                .bind(uri).bind(reference["locators"].to_string()).execute(&mut *connection).await?;
        }
        Ok(())
    }

    pub(crate) async fn connect_adopt_database_on(
        connection: &mut SqliteConnection,
        snapshot: &mut SqliteConnection,
    ) -> LibraryResult<()> {
        let current: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
                .fetch_one(&mut *connection)
                .await?;
        let (enabled, actor, clock, publisher): (bool, String, i64, Option<String>) =
            sqlx::query_as(
                "SELECT enabled,actor,clock,publisher FROM connect_profile WHERE singleton=1",
            )
            .fetch_one(&mut *connection)
            .await?;
        sqlx::query("ATTACH DATABASE ?1 AS receiving")
            .bind(current)
            .execute(&mut *snapshot)
            .await?;
        let mut tx = snapshot.begin().await?;
        // The profile carries no device-local file rows, so this device keeps its own. An accepted
        // file the profile has no track for is left out, and the next walk reads it.
        sqlx::raw_sql("INSERT INTO files(source_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,parse_version,picture_index,artist_pictures,playlist_identity,state)
                SELECT source.source_key,file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.native_id,file.revision,file.parse_version,file.picture_index,file.artist_pictures,file.playlist_identity,file.state
                FROM receiving.files file JOIN receiving.sources old USING(source_key) JOIN main.sources source ON source.object_id=old.object_id
                WHERE NOT(file.root LIKE 'http://%' OR file.root LIKE 'https://%' OR file.root LIKE 'smb://%')
                AND(file.state<>'accepted' OR file.kind IN('image','directory') OR EXISTS(SELECT 1 FROM main.track_details details JOIN main.tracks track USING(track_key)
                    WHERE track.source_key=source.source_key AND(details.source_path=file.path OR details.cue_path=file.path)));
                INSERT INTO file_inputs(file_key,position,path) SELECT file.file_key,input.position,input.path
                FROM receiving.file_inputs input JOIN receiving.files old USING(file_key) JOIN receiving.sources old_source ON old_source.source_key=old.source_key
                JOIN main.sources source ON source.object_id=old_source.object_id JOIN main.files file ON file.source_key=source.source_key AND file.path=old.path
                WHERE NOT(old.root LIKE 'http://%' OR old.root LIKE 'https://%' OR old.root LIKE 'smb://%');
                INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)
                SELECT source.source_key,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM receiving.local_locators locator LEFT JOIN receiving.sources old USING(source_key)
                LEFT JOIN main.sources source ON source.object_id=old.object_id WHERE EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=locator.media_uri);
                INSERT INTO downloads SELECT * FROM receiving.downloads;
                INSERT INTO playlist_file_settings SELECT settings.* FROM receiving.playlist_file_settings settings WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=settings.source_id);
                INSERT INTO playlist_file_ignored SELECT ignored.* FROM receiving.playlist_file_ignored ignored WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=ignored.source_id);
                INSERT INTO playlist_file_links(playlist_key,source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error)
                SELECT playlist.playlist_key,link.source_id,link.path,link.auto_refresh,link.auto_save,link.path_mode,link.revision,link.dirty,link.error
                FROM receiving.playlist_file_links link JOIN receiving.playlists old USING(playlist_key) LEFT JOIN receiving.sources old_source ON old_source.source_key=old.source_key
                LEFT JOIN main.sources source ON source.object_id=old_source.object_id JOIN main.playlists playlist ON playlist.source_key IS source.source_key AND playlist.object_id=old.object_id
                WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=link.source_id);
                INSERT OR IGNORE INTO artwork_previews SELECT * FROM receiving.artwork_previews;
                INSERT OR IGNORE INTO loudness SELECT * FROM receiving.loudness;
                INSERT OR IGNORE INTO album_loudness SELECT * FROM receiving.album_loudness;
                INSERT OR IGNORE INTO waveform SELECT * FROM receiving.waveform;")
                .execute(&mut *tx).await?;
        sqlx::query("UPDATE connect_profile SET enabled=?1,actor=?2,clock=max(clock,?3),publisher=?4 WHERE singleton=1").bind(enabled).bind(actor).bind(clock).bind(publisher).execute(&mut *tx).await?;
        tx.commit().await?;
        sqlx::query("DETACH DATABASE receiving")
            .execute(&mut *snapshot)
            .await?;
        crate::db::copy_database(snapshot, connection).await?;
        Ok(())
    }
}
