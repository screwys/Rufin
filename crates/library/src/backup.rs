//! Fixed semantic data export and transactional restore.
use crate::{Database, LibraryError, LibraryResult, PlaylistKey};
use sqlx::Connection;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateGroups {
    pub playlists: bool,
    pub favorites: bool,
    pub local_imports: bool,
    pub activity: bool,
    pub queue: bool,
}

impl StateGroups {
    pub fn members(self) -> impl Iterator<Item = &'static str> {
        [
            ("playlist-order.jsonl", self.playlists),
            ("playlist-folders.jsonl", self.playlists),
            ("smart.jsonl", self.playlists),
            ("user-state.jsonl", self.favorites),
            ("local-locators.jsonl", self.local_imports),
            ("activity.jsonl", self.activity),
            ("legacy-activity.jsonl", self.activity),
            ("queue.jsonl", self.queue),
        ]
        .into_iter()
        .filter_map(|(name, present)| present.then_some(name))
    }
}

#[derive(Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BackupRestoreReport {
    pub playlists: u64,
    pub playlist_entries: u64,
    pub skipped_playlist_entries: u64,
    pub smart_playlists: u64,
    pub user_states: u64,
    pub listens: u64,
    pub skipped_listens: u64,
    pub local_locators: u64,
    /// Database groups are committed even if explicitly selected external writes fail.
    pub warnings: Vec<String>,
}

fn private_file(path: &Path) -> LibraryResult<BufWriter<File>> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(BufWriter::new(options.open(path)?))
}
impl Database {
    pub async fn export_state(
        &self,
        directory: &Path,
        contents: StateGroups,
    ) -> LibraryResult<u64> {
        fs::create_dir(directory.join("playlists"))?;
        let (_export, mut connection) = self.acquire_export().await?;
        let mut transaction = connection.begin().await?;
        let mut ordinal = 0;
        if contents.playlists {
            let mut output = private_file(&directory.join("playlist-order.jsonl"))?;
            crate::playlists::export_playlist_order_jsonl_on(&mut transaction, &mut output).await?;
            output.flush()?;
            let mut cursor = -1;
            while let Some(key) = sqlx::query_scalar::<_, PlaylistKey>(
                "SELECT playlist_key FROM main.playlists WHERE playlist_key>?1 AND owner='rufin' AND name IS NOT NULL ORDER BY playlist_key LIMIT 1")
                .bind(cursor).fetch_optional(&mut *transaction).await? {
                let path = directory.join(format!("playlists/{ordinal}.m3u8"));
                let mut output = private_file(&path)?;
                crate::playlist_format::export_playlist_on(&mut transaction, key, &path, crate::PlaylistPathMode::Absolute, &mut output).await?;
                output.flush()?;
                cursor = key.raw();
                ordinal += 1;
            }
            let mut output = private_file(&directory.join("playlist-folders.jsonl"))?;
            crate::playlist_folders::export_on(&mut transaction, &mut output).await?;
            output.flush()?;
            let mut output = private_file(&directory.join("smart.jsonl"))?;
            crate::smart_playlists::export_smart_playlists_jsonl_on(&mut transaction, &mut output)
                .await?;
            output.flush()?;
        }
        if contents.favorites {
            let mut output = private_file(&directory.join("user-state.jsonl"))?;
            crate::favorites::export_user_media_state_jsonl_on(&mut transaction, &mut output)
                .await?;
            output.flush()?;
        }
        if contents.queue {
            let mut output = private_file(&directory.join("queue.jsonl"))?;
            crate::queue::export_queue_jsonl_on(&mut transaction, &mut output).await?;
            output.flush()?;
        }
        if contents.activity {
            let mut output = private_file(&directory.join("activity.jsonl"))?;
            crate::activity::export_activity_jsonl_on(&mut transaction, &mut output, None).await?;
            output.flush()?;
            let mut output = private_file(&directory.join("legacy-activity.jsonl"))?;
            crate::activity::export_legacy_activity_jsonl_on(&mut transaction, &mut output).await?;
            output.flush()?;
        }
        if contents.local_imports {
            let mut output = private_file(&directory.join("local-locators.jsonl"))?;
            crate::local::export_local_locators_jsonl_on(&mut transaction, &mut output).await?;
            output.flush()?;
        }
        transaction.commit().await?;
        drop(connection);
        Ok(ordinal)
    }

    /// Replace selected groups in one transaction; the caller intersects requested/present groups.
    pub async fn restore_state(
        &self,
        directory: &Path,
        contents: StateGroups,
        playlist_count: u64,
    ) -> LibraryResult<BackupRestoreReport> {
        let open = |name: &str| -> LibraryResult<BufReader<File>> {
            Ok(BufReader::new(File::open(directory.join(name))?))
        };
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut report = BackupRestoreReport::default();
        sync_restore_on(&mut transaction, contents, true).await?;
        let receiving: bool =
            sqlx::query_scalar("SELECT receiving FROM connect_profile WHERE singleton=1")
                .fetch_one(&mut *transaction)
                .await?;
        crate::connect::set_receiving_on(&mut transaction, true).await?;
        if contents.playlists {
            sqlx::raw_sql("DELETE FROM playlist_folders;DELETE FROM main.playlists WHERE owner='rufin';DELETE FROM smart_playlists;DELETE FROM playlist_file_ignored;DELETE FROM playlist_file_settings;")
                .execute(&mut *transaction)
                .await?;
            report.playlists = crate::playlists::import_playlists_jsonl_on(
                &mut transaction,
                open("playlist-order.jsonl")?,
            )
            .await?;
            for ordinal in 0..playlist_count {
                let name = format!("playlists/{ordinal}.m3u8");
                let path = directory.join(&name);
                let imported = crate::playlist_format::import_playlist_on(
                    &mut transaction,
                    crate::PlaylistFile::read(open(&name)?, &path)?,
                    &path,
                    None,
                    |_| None,
                )
                .await?;
                report.playlist_entries += imported.imported;
                report.skipped_playlist_entries += imported.skipped;
            }
            report.smart_playlists = crate::smart_playlists::import_smart_playlists_jsonl_on(
                &mut transaction,
                open("smart.jsonl")?,
            )
            .await?;
        }
        if contents.playlists && directory.join("playlist-folders.jsonl").exists() {
            crate::playlist_folders::import_on(&mut transaction, open("playlist-folders.jsonl")?)
                .await?;
        }

        if contents.favorites {
            sqlx::raw_sql("DELETE FROM user_media_state;DELETE FROM favorite_outbox;")
                .execute(&mut *transaction)
                .await?;
            report.user_states = crate::favorites::import_user_media_state_jsonl_on(
                &mut transaction,
                open("user-state.jsonl")?,
            )
            .await?;
        }
        if contents.queue {
            sqlx::raw_sql("DELETE FROM queue_state;DELETE FROM queue_positions;DELETE FROM queue_entries;DELETE FROM queue_contexts;")
                .execute(&mut *transaction)
                .await?;
            crate::queue::import_queue_jsonl_on(&mut transaction, open("queue.jsonl")?).await?;
        }
        if contents.activity {
            sqlx::raw_sql("DELETE FROM listens;DELETE FROM legacy_activity;")
                .execute(&mut *transaction)
                .await?;
            let activity = crate::activity::import_activity_jsonl_on(
                &mut transaction,
                open("activity.jsonl")?,
            )
            .await?;
            report.listens = activity.accepted;
            report.skipped_listens = activity.skipped;
            crate::activity::import_legacy_activity_jsonl_on(
                &mut transaction,
                open("legacy-activity.jsonl")?,
            )
            .await?;
        }
        if contents.local_imports {
            sqlx::query("DELETE FROM main.local_locators WHERE origin<>'download'")
                .execute(&mut *transaction)
                .await?;
            report.local_locators =
                crate::local::import_local_locators_jsonl_preserving_downloads_on(
                    &mut transaction,
                    open("local-locators.jsonl")?,
                )
                .await?;
        }
        crate::connect::set_receiving_on(&mut transaction, receiving).await?;
        sync_restore_on(&mut transaction, contents, false).await?;
        transaction.commit().await?;
        Ok(report)
    }
}

async fn sync_restore_on(
    connection: &mut sqlx::SqliteConnection,
    groups: StateGroups,
    deleting: bool,
) -> LibraryResult<()> {
    let active:bool=sqlx::query_scalar("SELECT profile IS NOT NULL AND enabled AND NOT receiving FROM connect_profile WHERE singleton=1").fetch_one(&mut *connection).await?;
    if !active {
        return Ok(());
    }
    let mut selected = Vec::new();
    if groups.playlists {
        selected.push("SELECT 'playlist' kind,json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id) object_key,playlist_key FROM playlists playlist WHERE owner='rufin'");
        selected.push(
            "SELECT 'smart' kind,object_id object_key,NULL playlist_key FROM smart_playlists",
        );
    }
    if groups.favorites {
        selected.push(
            "SELECT 'state' kind,media_uri object_key,NULL playlist_key FROM user_media_state",
        );
    }
    if groups.activity {
        selected.push("SELECT 'listen' kind,coalesce(external_id,'legacy-'||media_uri||'-'||started_at||'-'||listen_key) object_key,NULL playlist_key FROM listens");
        selected.push("SELECT 'legacy_activity' kind,json_array(source_id,period,item_kind,track_object_id) object_key,NULL playlist_key FROM legacy_activity");
    }
    if selected.is_empty() {
        return Ok(());
    }
    let query = format!(
        "SELECT kind,object_key,playlist_key FROM ({}) WHERE (kind,object_key)>(?1,?2) ORDER BY kind,object_key LIMIT ?3",
        selected.join(" UNION ALL ")
    );
    let mut after = (String::new(), String::new());
    loop {
        let page: Vec<(String, String, Option<PlaylistKey>)> =
            sqlx::query_as(sqlx::AssertSqlSafe(query.clone()))
                .bind(&after.0)
                .bind(&after.1)
                .bind(crate::CONNECT_PAGE_SIZE as i64)
                .fetch_all(&mut *connection)
                .await?;
        let Some(last) = page.last() else { break };
        after = (last.0.clone(), last.1.clone());
        let mut records = Vec::new();
        for (kind, key, playlist) in page {
            if crate::connect_current_kind(&kind) {
                crate::connect::mark_current_on(connection, &kind, &key, deleting).await?;
                continue;
            }
            if deleting {
                records.push(crate::ConnectRecord {
                    kind: kind.clone(),
                    key: key.clone(),
                    value: None,
                });
            } else {
                records.push(Database::connect_record_on(connection, &kind, &key).await?);
                if let Some(playlist) = playlist {
                    records.extend(
                        crate::playlists::playlist_entry_records_on(
                            connection,
                            playlist,
                            0,
                            i64::MAX,
                        )
                        .await?,
                    );
                }
            }
            if kind == "playlist" {
                records.push(if deleting {
                    crate::ConnectRecord {
                        kind: "playlist_artwork".into(),
                        key,
                        value: None,
                    }
                } else {
                    Database::connect_record_on(connection, "playlist_artwork", &key).await?
                });
            }
        }
        crate::profile::write_local_on(connection, &records).await?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn database(directory: &Path, name: &str) -> Database {
        Database::open(directory.join(name)).await.unwrap()
    }
    async fn seed(database: &Database, rating: i64) {
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES('https://example.test/song',1,?1)").bind(rating).execute(&mut *connection).await.unwrap();
    }
    async fn rating(database: &Database) -> i64 {
        let mut writer = database.writer().await.unwrap();
        sqlx::query_scalar("SELECT rating FROM user_media_state LIMIT 1")
            .fetch_one(writer.as_mut().unwrap())
            .await
            .unwrap()
    }

    fn all_groups() -> StateGroups {
        StateGroups {
            playlists: true,
            favorites: true,
            local_imports: true,
            activity: true,
            queue: true,
        }
    }
    async fn exported(database: &Database) -> (tempfile::TempDir, u64) {
        let directory = tempfile::tempdir().unwrap();
        let count = database
            .export_state(directory.path(), all_groups())
            .await
            .unwrap();
        (directory, count)
    }
    #[tokio::test]
    async fn export_runs_alongside_browsing_and_playback_and_releases_its_reader() {
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path(), "export").await;
        let (_permit, _foreground) = database
            .acquire_general(&crate::ReadCancellation::new())
            .await
            .unwrap();
        let mut playback = database.acquire_reader().await.unwrap();
        let output = tempfile::tempdir().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            database.export_state(output.path(), all_groups()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&mut *playback)
                .await
                .unwrap(),
            1
        );
        let _reader =
            tokio::time::timeout(std::time::Duration::from_secs(2), database.acquire_export())
                .await
                .expect("Export must return its reader before archive output begins")
                .unwrap();
    }
    #[tokio::test]
    async fn selected_restore_is_atomic_and_preserves_downloads() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let target = database(directory.path(), "target").await;
        seed(&source, 73).await;
        seed(&target, 19).await;
        for (database, path) in [(&source, "/old/song.flac"), (&target, "/current/song.flac")] {
            let mut writer = database.writer().await.unwrap();
            sqlx::query("INSERT INTO main.local_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES('https://example.test/song','download',?1,'/','song.flac','file:///song.flac')").bind(path).execute(writer.as_mut().unwrap()).await.unwrap();
        }
        let (staged, playlist_count) = exported(&source).await;
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::query("CREATE TRIGGER reject_restore BEFORE INSERT ON user_media_state BEGIN SELECT RAISE(ABORT,'selected write failed'); END")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        assert!(
            target
                .restore_state(staged.path(), all_groups(), playlist_count)
                .await
                .is_err()
        );
        assert_eq!(rating(&target).await, 19);
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::query("DROP TRIGGER reject_restore")
                .execute(writer.as_mut().unwrap())
                .await
                .unwrap();
        }
        let report = target
            .restore_state(staged.path(), all_groups(), playlist_count)
            .await
            .unwrap();
        assert_eq!(report.user_states, 1);
        assert_eq!(rating(&target).await, 73);
        let mut writer = target.writer().await.unwrap();
        let pending: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM favorite_outbox)+(SELECT count(*) FROM listen_outbox)",
        )
        .fetch_one(writer.as_mut().unwrap())
        .await
        .unwrap();
        assert_eq!(pending, 0);
        let download: String =
            sqlx::query_scalar("SELECT path FROM main.local_locators WHERE origin='download'")
                .fetch_one(writer.as_mut().unwrap())
                .await
                .unwrap();
        assert_eq!(download, "/current/song.flac");
    }
    #[tokio::test]
    async fn empty_catalog_roundtrip_preserves_native_rank_and_authored_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let target = database(directory.path(), "target").await;
        {
            let mut writer = source.writer().await.unwrap();
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id) VALUES(1,'missing-source');INSERT INTO playlists(playlist_key,source_key,owner,object_id,name,position) VALUES(1,1,'source','native',NULL,0),(2,NULL,'rufin','authored','Mine',1);INSERT INTO tracks(track_key,media_uri,metadata) VALUES(1,'https://example.test/song',jsonb(json_object('title','Saved')));INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) VALUES(2,'a',1,0),(2,'b',1,1);INSERT INTO listens(listen_key,external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(7,NULL,'missing-source','https://example.test/song','Saved','Artist','Album',1,'2026-09',100,90);").execute(writer.as_mut().unwrap()).await.unwrap();
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id) VALUES(2,'local');INSERT INTO playlists(playlist_key,source_key,object_id,name,position) VALUES(3,2,'local-authored','Local playlist',2);INSERT INTO tracks(track_key,media_uri,metadata) VALUES(2,'file:///music/song.flac',jsonb(json_object('title','Local saved')));INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) VALUES(3,'c',2,0);")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        let smart = source
            .create_smart_playlist("Saved choice", &crate::SmartPlaylistDefinition::default())
            .await
            .unwrap();
        let mut expected_artwork = Vec::new();
        {
            let mut writer = source.writer().await.unwrap();
            let connection = writer.as_mut().unwrap();
            let owners: Vec<(String,Option<String>,String)> = sqlx::query_as("SELECT 'playlists',source.object_id,playlist.object_id FROM playlists playlist LEFT JOIN sources source USING(source_key) UNION ALL SELECT 'smart_playlists',NULL,object_id FROM smart_playlists WHERE smart_playlist_key=?1")
                .bind(smart).fetch_all(&mut *connection).await.unwrap();
            for (table, source_id, object_id) in owners {
                let binding = serde_json::to_vec(&[crate::ArtworkCandidate::new(
                    crate::ArtworkImage::Playlist {
                        smart: table == "smart_playlists",
                        source_id,
                        object_id: object_id.clone(),
                        revision: "auto:saved".into(),
                        automatic: Some(crate::PlaylistArtworkManifest {
                            source_id: None,
                            folder_id: None,
                            chosen: vec!["https://example.test/song".into()],
                            pending: false,
                        }),
                        resolved: None,
                    },
                    None,
                )])
                .unwrap();
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET artwork_binding=?1 WHERE object_id=?2"
                )))
                .bind(&binding)
                .bind(&object_id)
                .execute(&mut *connection)
                .await
                .unwrap();
                expected_artwork.push((table, object_id, binding));
            }
        }
        let admitted = source
            .queue_entries(
                crate::QueueLoad::items(
                    (0..250)
                        .map(|_| {
                            (
                                crate::TrackReference::direct(
                                    "https://example.test/song",
                                    "Saved",
                                    "",
                                    "",
                                    100,
                                ),
                                crate::QueueProvenance::Manual,
                            )
                        })
                        .collect(),
                ),
                None,
            )
            .await
            .unwrap();
        source
            .save_queue(&crate::QueueRestore {
                entries: admitted.entries.into(),
                order: (0..250).collect(),
                repeat_mode: crate::QueueRepeatMode::All,
                shuffled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut original_queue = Vec::new();
        source
            .export_queue_jsonl(&mut original_queue)
            .await
            .unwrap();
        let (staged, playlist_count) = exported(&source).await;
        assert_eq!(playlist_count, 2);
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'catalog','Catalog','catalog',zeroblob(32)); INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,local_play_count,indexed,native_metadata) VALUES(1,'song','https://example.test/song',jsonb(json_object('title','Song','sort_text','song','duration_millis',100)),'song',7,1,1);").execute(writer.as_mut().unwrap()).await.unwrap();
        }
        target
            .restore_state(staged.path(), all_groups(), playlist_count)
            .await
            .unwrap();
        for (table, object, binding) in expected_artwork {
            let restored: Vec<u8> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT artwork_binding FROM {table} WHERE object_id=?1"
            )))
            .bind(object)
            .fetch_one(&mut *target.acquire_reader().await.unwrap())
            .await
            .unwrap();
            let restored: Vec<crate::ArtworkCandidate> = serde_json::from_slice(&restored).unwrap();
            let expected: Vec<crate::ArtworkCandidate> = serde_json::from_slice(&binding).unwrap();
            let restored = restored
                .into_iter()
                .find(crate::ArtworkCandidate::is_automatic_playlist)
                .unwrap();
            let expected = expected
                .into_iter()
                .find(crate::ArtworkCandidate::is_automatic_playlist)
                .unwrap();
            assert_eq!(restored.asset_identity(), expected.asset_identity());
            let (
                crate::ArtworkImage::Playlist {
                    automatic: Some(restored),
                    ..
                },
                crate::ArtworkImage::Playlist {
                    automatic: Some(expected),
                    ..
                },
            ) = (restored.image, expected.image)
            else {
                unreachable!()
            };
            assert_eq!(
                (restored.source_id, restored.folder_id, restored.chosen),
                (expected.source_id, expected.folder_id, expected.chosen)
            );
        }
        let mut restored_queue = Vec::new();
        target
            .export_queue_jsonl(&mut restored_queue)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                original_queue.split(|byte| *byte == b'\n').next().unwrap()
            )
            .unwrap(),
            serde_json::from_slice::<serde_json::Value>(
                restored_queue.split(|byte| *byte == b'\n').next().unwrap()
            )
            .unwrap(),
        );
        let mut writer = target.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT local_play_count FROM tracks WHERE media_uri='https://example.test/song'"
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap(),
            1
        );
        let identities: Vec<(String, Option<String>, i64)> =
            sqlx::query_as("SELECT object_id,name,position FROM main.playlists ORDER BY position")
                .fetch_all(&mut *connection)
                .await
                .unwrap();
        assert_eq!(
            identities,
            vec![
                ("native".into(), None, 0),
                ("authored".into(), Some("Mine".into()), 1),
                ("local-authored".into(), Some("Local playlist".into()), 2)
            ]
        );
        let entries: Vec<(String, String)> = sqlx::query_as(
            "SELECT entry.object_id,track.title FROM playlist_entries entry JOIN tracks track USING(track_key) ORDER BY entry.playlist_key,entry.position",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(
            entries,
            vec![
                ("a".into(), "Song".into()),
                ("b".into(), "Song".into()),
                ("c".into(), "Local saved".into())
            ]
        );
        let listen: (i64, Option<String>, String) =
            sqlx::query_as("SELECT listen_key,external_id,source_id FROM listens")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(listen, (7, None, "missing-source".into()));
    }
    #[tokio::test]
    async fn selected_groups_preserve_catalog_and_absent_groups_but_replace_empty_groups() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let target = database(directory.path(), "target").await;
        seed(&source, 73).await;
        seed(&target, 19).await;
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (83,'remote','Remote','remote',zeroblob(32));INSERT INTO main.playlists(object_id,name,position) VALUES('retained','Keep me',0);")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        let favorites = StateGroups {
            playlists: false,
            favorites: true,
            local_imports: false,
            activity: false,
            queue: false,
        };
        let staged = tempfile::tempdir().unwrap();
        let count = source.export_state(staged.path(), favorites).await.unwrap();
        target
            .restore_state(staged.path(), favorites, count)
            .await
            .unwrap();
        assert_eq!(rating(&target).await, 73);
        {
            let mut writer = target.writer().await.unwrap();
            let connection = writer.as_mut().unwrap();
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT object_id FROM sources")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap(),
                "remote"
            );
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT name FROM main.playlists")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap(),
                "Keep me"
            );
        }
        let empty = database(directory.path(), "empty").await;
        let staged = tempfile::tempdir().unwrap();
        let count = empty.export_state(staged.path(), favorites).await.unwrap();
        target
            .restore_state(staged.path(), favorites, count)
            .await
            .unwrap();
        let mut writer = target.writer().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_media_state")
                .fetch_one(writer.as_mut().unwrap())
                .await
                .unwrap(),
            0
        );
    }
}
