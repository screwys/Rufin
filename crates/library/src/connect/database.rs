//! Whole-Store profile copies for joining a profile.
use super::*;
use sqlx::Connection;

impl Catalog {
    pub(crate) async fn connect_prepare_export_on(
        connection: &mut SqliteConnection,
    ) -> LibraryResult<()> {
        let sources: Vec<String> = sqlx::query_scalar("SELECT DISTINCT source.object_id FROM local_file_access_roots access JOIN sources source USING(source_key)")
            .fetch_all(&mut *connection).await?;
        for source in sources {
            for root in read_roots(connection, &source).await? {
                sqlx::query("INSERT INTO connect_roots(source_id,id,label) VALUES(?1,?2,?3) ON CONFLICT(source_id,id) DO UPDATE SET label=excluded.label WHERE label IS NOT excluded.label")
                    .bind(&source).bind(root.id).bind(root.label).execute(&mut *connection).await?;
            }
        }
        sqlx::raw_sql("DELETE FROM queue_state; DELETE FROM queue_entries; DELETE FROM queue_contexts; DELETE FROM queue_transfer_pages;
            DELETE FROM outbox; DELETE FROM listen_outbox;
            DELETE FROM local_locators; DELETE FROM local_access_metadata; DELETE FROM downloads;
            DELETE FROM playlist_file_links; DELETE FROM playlist_file_ignored; DELETE FROM playlist_file_settings;
            DELETE FROM waveform_cache WHERE NOT EXISTS(SELECT 1 FROM tracks JOIN track_details details USING(track_key) WHERE tracks.media_uri=waveform_cache.media_uri AND details.source_loudness_analysis_key=waveform_cache.analysis_key);
            UPDATE track_details SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
            UPDATE albums SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
            DELETE FROM loudness_measurements WHERE(entity_kind='track' AND NOT EXISTS(SELECT 1 FROM track_details WHERE track_key=entity_key AND loudness_analysis_key=analysis_key)) OR(entity_kind='album' AND NOT EXISTS(SELECT 1 FROM albums WHERE album_key=entity_key AND loudness_analysis_key=analysis_key));
            DELETE FROM local_file_observations WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
            DELETE FROM local_file_access_roots WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
            UPDATE connect_profile SET enabled=0;")
            .execute(connection).await?;
        Ok(())
    }

    /// Downloaded Connect media the incoming profile may not know, with the local rows that
    /// follow each track.
    pub(crate) async fn downloaded_references_on(
        connection: &mut SqliteConnection,
    ) -> LibraryResult<Vec<Value>> {
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT json_object('media_uri',track.media_uri,'metadata',json({}),'artwork',CASE WHEN details.native_artwork IS NULL THEN NULL ELSE hex(details.native_artwork) END,'source_loudness',hex(details.source_loudness_analysis_key),'loudness',hex(details.loudness_analysis_key),'source_path',details.source_path,'cue_path',details.cue_path,'cue_start_millis',details.cue_start_millis,'cue_end_millis',details.cue_end_millis,
            'locators',(SELECT json_group_array(json_object('source_id',source.object_id,'origin',locator.origin,'path',locator.path,'root',locator.root,'relative_path',locator.relative_path,'access_uri',locator.access_uri)) FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE locator.media_uri=track.media_uri),
            'access',(SELECT json_group_array(json_object('access_uri',metadata.access_uri,'size_bytes',metadata.size_bytes,'mtime_ns',metadata.mtime_ns,'device_id',metadata.device_id,'inode',metadata.inode,'parser_version',metadata.parser_version,'loudness_analysis_key',CASE WHEN metadata.loudness_analysis_key IS NULL THEN NULL ELSE hex(metadata.loudness_analysis_key) END)) FROM local_access_metadata metadata WHERE metadata.access_uri IN(SELECT access_uri FROM local_locators WHERE media_uri=track.media_uri)),
            'waveform',(SELECT peaks FROM waveform_cache WHERE media_uri=track.media_uri AND analysis_key=details.loudness_analysis_key),
            'measurement',(SELECT json_array(integrated_lufs,true_peak) FROM loudness_measurements WHERE entity_kind='track' AND entity_key=track.track_key AND analysis_key=details.loudness_analysis_key))
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
        // Device observations keep their own stat values under the shared file identity.
        sqlx::raw_sql("DELETE FROM local_file_observations WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
                DELETE FROM local_file_access_roots WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
                INSERT OR IGNORE INTO local_file_access_roots(source_key,root,root_id)
                SELECT source.source_key,access.root,access.root_id FROM receiving.local_file_access_roots access JOIN receiving.sources old USING(source_key) JOIN main.sources source ON source.object_id=old.object_id
                WHERE NOT(access.root LIKE 'http://%' OR access.root LIKE 'https://%' OR access.root LIKE 'smb://%');
                INSERT INTO local_file_observations(local_file_key,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,native_id,revision,observed_stamp,parent_path,parsed_stamp,accepted_facts,state)
                SELECT file.local_file_key,access.path,access.root,access.relative_path,access.size_bytes,access.mtime_ns,access.device_id,access.inode,access.native_id,access.revision,0,access.parent_path,
                    CASE WHEN old_file.kind='directory' AND json_extract(access.parsed_stamp,'$[4]')=access.observed_stamp THEN json_set(access.parsed_stamp,'$[4]',0) ELSE access.parsed_stamp END,access.accepted_facts,access.state
                FROM receiving.local_file_observations access JOIN receiving.local_files old_file USING(local_file_key)
                JOIN receiving.sources old ON old.source_key=old_file.source_key JOIN main.sources source ON source.object_id=old.object_id
                JOIN main.local_files file ON file.source_key=source.source_key AND file.root_id=old_file.root_id AND file.relative_path=old_file.relative_path
                WHERE NOT(access.root LIKE 'http://%' OR access.root LIKE 'https://%' OR access.root LIKE 'smb://%')
                ON CONFLICT(local_file_key) DO UPDATE SET path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,observed_stamp=excluded.observed_stamp,parent_path=excluded.parent_path,parsed_stamp=excluded.parsed_stamp,accepted_facts=excluded.accepted_facts,state=excluded.state;
                INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)
                SELECT source.source_key,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM receiving.local_locators locator LEFT JOIN receiving.sources old USING(source_key)
                LEFT JOIN main.sources source ON source.object_id=old.object_id WHERE EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=locator.media_uri);
                INSERT INTO local_access_metadata SELECT metadata.* FROM receiving.local_access_metadata metadata WHERE EXISTS(SELECT 1 FROM main.local_locators WHERE access_uri=metadata.access_uri);
                INSERT INTO downloads SELECT * FROM receiving.downloads;
                INSERT INTO playlist_file_settings SELECT settings.* FROM receiving.playlist_file_settings settings WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=settings.source_id);
                INSERT INTO playlist_file_ignored SELECT ignored.* FROM receiving.playlist_file_ignored ignored WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=ignored.source_id);
                INSERT INTO playlist_file_links(playlist_key,source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error)
                SELECT playlist.playlist_key,link.source_id,link.path,link.auto_refresh,link.auto_save,link.path_mode,link.revision,link.dirty,link.error
                FROM receiving.playlist_file_links link JOIN receiving.playlists old USING(playlist_key) LEFT JOIN receiving.sources old_source ON old_source.source_key=old.source_key
                LEFT JOIN main.sources source ON source.object_id=old_source.object_id JOIN main.playlists playlist ON playlist.source_key IS source.source_key AND playlist.object_id=old.object_id
                WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=link.source_id);
                UPDATE track_details SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
                UPDATE track_details SET loudness_analysis_key=old.loudness_analysis_key FROM receiving.track_details old JOIN receiving.tracks old_track ON old_track.track_key=old.track_key JOIN main.tracks track ON track.media_uri=old_track.media_uri WHERE track_details.track_key=track.track_key AND track_details.source_loudness_analysis_key=old.source_loudness_analysis_key;
                UPDATE albums SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
                UPDATE albums SET loudness_analysis_key=old.loudness_analysis_key FROM receiving.albums old WHERE albums.media_uri=old.media_uri AND albums.source_loudness_analysis_key=old.source_loudness_analysis_key;
                INSERT OR IGNORE INTO artwork_previews SELECT * FROM receiving.artwork_previews;
                INSERT OR IGNORE INTO waveform_cache SELECT result.* FROM receiving.waveform_cache result JOIN main.tracks track USING(media_uri) JOIN main.track_details details USING(track_key) WHERE result.analysis_key=details.loudness_analysis_key;
                INSERT OR IGNORE INTO loudness_measurements SELECT track.source_key,'track',track.track_key,measurement.analysis_key,measurement.integrated_lufs,measurement.true_peak
                FROM receiving.loudness_measurements measurement JOIN receiving.tracks old ON measurement.entity_kind='track' AND old.track_key=measurement.entity_key JOIN main.tracks track USING(media_uri) JOIN main.track_details details USING(track_key)
                WHERE measurement.analysis_key=details.loudness_analysis_key;
                INSERT OR IGNORE INTO loudness_measurements SELECT album.source_key,'album',album.album_key,measurement.analysis_key,measurement.integrated_lufs,measurement.true_peak
                FROM receiving.loudness_measurements measurement JOIN receiving.albums old ON measurement.entity_kind='album' AND old.album_key=measurement.entity_key JOIN main.albums album USING(media_uri)
                WHERE measurement.analysis_key=album.loudness_analysis_key;")
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
