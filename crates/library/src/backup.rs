//! Fixed semantic data export and transactional restore.
use crate::{Database, LibraryError, LibraryResult, PlaylistKey};
use sqlx::Connection;
use std::fs::{self, File, OpenOptions};
use std::io::BufReader;
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

fn private_file(path: &Path) -> LibraryResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
impl Database {
    pub async fn export_state(
        &self,
        directory: &Path,
        contents: StateGroups,
    ) -> LibraryResult<u64> {
        fs::create_dir(directory.join("playlists"))?;
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let mut ordinal = 0;
        if contents.playlists {
            crate::playlists::export_playlist_order_jsonl_on(
                &mut transaction,
                private_file(&directory.join("playlist-order.jsonl"))?,
            )
            .await?;
            let mut cursor = -1;
            while let Some(key) = sqlx::query_scalar::<_, PlaylistKey>(
                "SELECT playlist_key FROM main.playlists WHERE playlist_key>?1 AND name IS NOT NULL ORDER BY playlist_key LIMIT 1")
                .bind(cursor).fetch_optional(&mut *transaction).await? {
                let path = directory.join(format!("playlists/{ordinal}.m3u8"));
                crate::m3u::export_playlist_m3u_on(&mut transaction, key, &path, private_file(&path)?).await?;
                cursor = key.raw();
                ordinal += 1;
            }
            crate::smart_playlists::export_smart_playlists_jsonl_on(
                &mut transaction,
                private_file(&directory.join("smart.jsonl"))?,
            )
            .await?;
        }
        if contents.favorites {
            crate::favorites::export_user_media_state_jsonl_on(
                &mut transaction,
                private_file(&directory.join("user-state.jsonl"))?,
            )
            .await?;
        }
        if contents.queue {
            crate::queue::export_queue_jsonl_on(
                &mut transaction,
                private_file(&directory.join("queue.jsonl"))?,
            )
            .await?;
        }
        if contents.activity {
            crate::activity::export_activity_jsonl_on(
                &mut transaction,
                private_file(&directory.join("activity.jsonl"))?,
                None,
            )
            .await?;
            crate::activity::export_legacy_activity_jsonl_on(
                &mut transaction,
                private_file(&directory.join("legacy-activity.jsonl"))?,
            )
            .await?;
        }
        if contents.local_imports {
            crate::local::export_local_locators_jsonl_on(
                &mut transaction,
                private_file(&directory.join("local-locators.jsonl"))?,
            )
            .await?;
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
        if contents.playlists {
            sqlx::raw_sql("DELETE FROM main.playlists;DELETE FROM smart_playlists;")
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
                let imported = crate::m3u::import_playlist_m3u_on(
                    &mut transaction,
                    open(&name)?,
                    &path,
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
            sqlx::raw_sql("DELETE FROM queue_occurrences;DELETE FROM queue_state;")
                .execute(&mut *transaction)
                .await?;
            crate::queue::import_queue_jsonl_on(&mut transaction, open("queue.jsonl")?).await?;
        }
        if contents.activity {
            sqlx::raw_sql("DELETE FROM listens;DELETE FROM legacy_activity;UPDATE tracks SET local_play_count=0 WHERE local_play_count<>0;")
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
        transaction.commit().await?;
        Ok(report)
    }
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
    async fn export_leaves_the_route_lane_free_and_releases_its_reader() {
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path(), "export").await;
        let (_permit, _foreground) = database
            .acquire_general(&crate::ReadCancellation::new())
            .await
            .unwrap();
        let output = tempfile::tempdir().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            database.export_state(output.path(), all_groups()),
        )
        .await
        .unwrap()
        .unwrap();
        let _reader =
            tokio::time::timeout(std::time::Duration::from_secs(2), database.acquire_reader())
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
            sqlx::raw_sql("INSERT INTO source_ids VALUES(1,'missing-source');INSERT INTO main.playlists(playlist_key,source_key,object_id,name,position) VALUES(1,1,'native',NULL,0),(2,NULL,'authored','Mine',1);INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,title,position) VALUES(2,'a','https://example.test/song','Saved',0),(2,'b','https://example.test/song','Saved duplicate',1);INSERT INTO listens(listen_key,external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(7,NULL,'missing-source','https://example.test/song','Saved','Artist','Album',1,'2026-09',100,90);").execute(writer.as_mut().unwrap()).await.unwrap();
            sqlx::raw_sql("INSERT INTO source_ids VALUES(2,'local');INSERT INTO main.playlists(playlist_key,source_key,object_id,name,position) VALUES(3,2,'local-authored','Local playlist',2);INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,title,position) VALUES(3,'c','file:///music/song.flac','Local saved',0);")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        source
            .save_queue(&crate::QueueRestore {
                entries: (0..250)
                    .map(|index| crate::QueueEntry {
                        occurrence: format!("backup:{index}").into(),
                        media_uri: "https://example.test/song".into(),
                        playlist_entry_id: None,
                        provenance: crate::QueueProvenance::Manual,
                    })
                    .collect(),
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
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(1,'catalog','Catalog','catalog',zeroblob(32),zeroblob(32)); INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,local_play_count) VALUES(1,'song','https://example.test/song','Song','song','','','song',100,7);").execute(writer.as_mut().unwrap()).await.unwrap();
        }
        target
            .restore_state(staged.path(), all_groups(), playlist_count)
            .await
            .unwrap();
        let mut restored_queue = Vec::new();
        target
            .export_queue_jsonl(&mut restored_queue)
            .await
            .unwrap();
        assert_eq!(original_queue, restored_queue);
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
            "SELECT object_id,title FROM main.playlist_entries ORDER BY playlist_key,position",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(
            entries,
            vec![
                ("a".into(), "Saved".into()),
                ("b".into(), "Saved duplicate".into()),
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
            sqlx::raw_sql("INSERT INTO catalog.sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(83,'remote','Remote','remote',zeroblob(32),zeroblob(32));INSERT INTO main.playlists(object_id,name,position) VALUES('retained','Keep me',0);")
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
                sqlx::query_scalar::<_, String>("SELECT object_id FROM catalog.sources")
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
