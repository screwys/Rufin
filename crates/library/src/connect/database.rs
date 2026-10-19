//! Indexed Connect snapshots keep SQLite's canonical keys and ready indexes.
use super::*;

impl Database {
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
        sqlx::raw_sql("DELETE FROM queue_state; DELETE FROM queue_positions; DELETE FROM queue_entries; DELETE FROM queue_contexts; DELETE FROM queue_transfer_pages;
            DELETE FROM favorite_outbox; DELETE FROM listen_outbox; DELETE FROM native_playlist_pending;
            DELETE FROM local_locators; DELETE FROM local_access_metadata; DELETE FROM connect_media_files;
            DELETE FROM playlist_file_links; DELETE FROM playlist_file_ignored; DELETE FROM playlist_file_settings;
            DELETE FROM waveform_cache WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE tracks.media_uri=waveform_cache.media_uri AND tracks.source_loudness_analysis_key=waveform_cache.analysis_key);
            UPDATE tracks SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
            UPDATE albums SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
            DELETE FROM loudness_measurements WHERE(entity_kind='track' AND NOT EXISTS(SELECT 1 FROM tracks WHERE track_key=entity_key AND loudness_analysis_key=analysis_key)) OR(entity_kind='album' AND NOT EXISTS(SELECT 1 FROM albums WHERE album_key=entity_key AND loudness_analysis_key=analysis_key));
            DELETE FROM local_file_observations WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
            DELETE FROM local_file_access_roots WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
            UPDATE connect_profile SET enabled=0,receiving=0;")
            .execute(connection).await?;
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
        let mut after = String::new();
        loop {
            let tracks: Vec<(String, String, Option<Vec<u8>>)> = sqlx::query_as("SELECT track.media_uri,coalesce(json(track.metadata),'{}'),track.native_artwork FROM receiving.tracks track WHERE track.media_uri>?1 AND EXISTS(SELECT 1 FROM receiving.connect_media_files file WHERE file.media_uri=track.media_uri) AND NOT EXISTS(SELECT 1 FROM main.tracks current WHERE current.media_uri=track.media_uri) ORDER BY track.media_uri LIMIT 128")
                    .bind(&after).fetch_all(&mut *tx).await?;
            let Some(last) = tracks.last() else {
                break;
            };
            after = last.0.clone();
            for (media_uri, metadata, artwork_binding) in tracks {
                let key = crate::catalog::remember_track(
                    &mut tx,
                    &crate::TrackReference {
                        media_uri,
                        metadata: serde_json::from_str(&metadata)?,
                        artwork_binding,
                    },
                )
                .await?;
                sqlx::query("UPDATE tracks SET metadata=old.metadata,source_loudness_analysis_key=old.source_loudness_analysis_key,loudness_analysis_key=old.loudness_analysis_key,source_path=old.source_path,cue_path=old.cue_path,cue_start_millis=old.cue_start_millis,cue_end_millis=old.cue_end_millis FROM receiving.tracks old WHERE tracks.track_key=?1 AND tracks.media_uri=old.media_uri")
                        .bind(key).execute(&mut *tx).await?;
            }
        }
        // Device observations keep their own stat values under the shared file identity.
        sqlx::raw_sql("DELETE FROM local_file_observations WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
                DELETE FROM local_file_access_roots WHERE NOT(root LIKE 'http://%' OR root LIKE 'https://%' OR root LIKE 'smb://%');
                INSERT OR IGNORE INTO local_file_access_roots(source_key,root,root_id)
                SELECT source.source_key,access.root,access.root_id FROM receiving.local_file_access_roots access JOIN receiving.sources old USING(source_key) JOIN main.sources source ON source.object_id=old.object_id
                WHERE NOT(access.root LIKE 'http://%' OR access.root LIKE 'https://%' OR access.root LIKE 'smb://%');
                UPDATE sources SET observation_stamp=observation_stamp+1;
                INSERT INTO local_file_observations(local_file_key,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,native_id,revision,observed_stamp,parent_path,parsed_stamp,accepted_facts,state)
                SELECT file.local_file_key,access.path,access.root,access.relative_path,access.size_bytes,access.mtime_ns,access.device_id,access.inode,access.native_id,access.revision,source.observation_stamp-1,access.parent_path,
                    CASE WHEN old_file.kind='directory' AND json_extract(access.parsed_stamp,'$[4]')=access.observed_stamp THEN json_set(access.parsed_stamp,'$[4]',source.observation_stamp-1) ELSE access.parsed_stamp END,access.accepted_facts,access.state
                FROM receiving.local_file_observations access JOIN receiving.local_files old_file USING(local_file_key)
                JOIN receiving.sources old ON old.source_key=old_file.source_key JOIN main.sources source ON source.object_id=old.object_id
                JOIN main.local_files file ON file.source_key=source.source_key AND file.root_id=old_file.root_id AND file.relative_path=old_file.relative_path
                WHERE NOT(access.root LIKE 'http://%' OR access.root LIKE 'https://%' OR access.root LIKE 'smb://%')
                ON CONFLICT(local_file_key) DO UPDATE SET path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,observed_stamp=excluded.observed_stamp,parent_path=excluded.parent_path,parsed_stamp=excluded.parsed_stamp,accepted_facts=excluded.accepted_facts,state=excluded.state;
                INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)
                SELECT source.source_key,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM receiving.local_locators locator LEFT JOIN receiving.sources old USING(source_key)
                LEFT JOIN main.sources source ON source.object_id=old.object_id WHERE EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=locator.media_uri);
                INSERT INTO local_access_metadata SELECT metadata.* FROM receiving.local_access_metadata metadata WHERE EXISTS(SELECT 1 FROM main.local_locators WHERE access_uri=metadata.access_uri);
                INSERT INTO connect_media_files SELECT * FROM receiving.connect_media_files;
                INSERT INTO playlist_file_settings SELECT settings.* FROM receiving.playlist_file_settings settings WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=settings.source_id);
                INSERT INTO playlist_file_ignored SELECT ignored.* FROM receiving.playlist_file_ignored ignored WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=ignored.source_id);
                INSERT INTO playlist_file_links(playlist_key,source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error)
                SELECT playlist.playlist_key,link.source_id,link.path,link.auto_refresh,link.auto_save,link.path_mode,link.revision,link.dirty,link.error
                FROM receiving.playlist_file_links link JOIN receiving.playlists old USING(playlist_key) LEFT JOIN receiving.sources old_source ON old_source.source_key=old.source_key
                LEFT JOIN main.sources source ON source.object_id=old_source.object_id JOIN main.playlists playlist ON playlist.source_key IS source.source_key AND playlist.object_id=old.object_id
                WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=link.source_id);
                UPDATE tracks SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
                UPDATE tracks SET loudness_analysis_key=old.loudness_analysis_key FROM receiving.tracks old WHERE tracks.media_uri=old.media_uri AND tracks.source_loudness_analysis_key=old.source_loudness_analysis_key;
                UPDATE albums SET loudness_analysis_key=source_loudness_analysis_key WHERE loudness_analysis_key<>source_loudness_analysis_key;
                UPDATE albums SET loudness_analysis_key=old.loudness_analysis_key FROM receiving.albums old WHERE albums.media_uri=old.media_uri AND albums.source_loudness_analysis_key=old.source_loudness_analysis_key;
                INSERT OR IGNORE INTO artwork_previews SELECT * FROM receiving.artwork_previews;
                INSERT OR IGNORE INTO waveform_cache SELECT result.* FROM receiving.waveform_cache result JOIN main.tracks track USING(media_uri) WHERE result.analysis_key=track.loudness_analysis_key;
                INSERT OR IGNORE INTO loudness_measurements SELECT track.source_key,'track',track.track_key,measurement.analysis_key,measurement.integrated_lufs,measurement.true_peak
                FROM receiving.loudness_measurements measurement JOIN receiving.tracks old ON measurement.entity_kind='track' AND old.track_key=measurement.entity_key JOIN main.tracks track USING(media_uri)
                WHERE measurement.analysis_key=track.loudness_analysis_key;
                INSERT OR IGNORE INTO loudness_measurements SELECT album.source_key,'album',album.album_key,measurement.analysis_key,measurement.integrated_lufs,measurement.true_peak
                FROM receiving.loudness_measurements measurement JOIN receiving.albums old ON measurement.entity_kind='album' AND old.album_key=measurement.entity_key JOIN main.albums album USING(media_uri)
                WHERE measurement.analysis_key=album.loudness_analysis_key;")
                .execute(&mut *tx).await?;
        sqlx::query("UPDATE connect_profile SET enabled=?1,actor=?2,clock=max(clock,?3),publisher=?4,receiving=0 WHERE singleton=1").bind(enabled).bind(actor).bind(clock).bind(publisher).execute(&mut *tx).await?;
        tx.commit().await?;
        sqlx::query("DETACH DATABASE receiving")
            .execute(&mut *snapshot)
            .await?;
        crate::db::copy_database(snapshot, connection).await?;
        Ok(())
    }
}
