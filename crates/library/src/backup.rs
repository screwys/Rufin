//! Streaming semantic backups. Archive framing is staged before any live owner is changed.
use crate::{Database, LibraryError, LibraryResult, PlaylistKey};
use chrono::{Datelike, TimeZone};
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const METADATA_LIMIT: u64 = 16 * 1024 * 1024;
fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::InvalidRequest(message.into())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupContents {
    pub settings: bool,
    pub saved_logins: bool,
    pub playlists: bool,
    pub favorites: bool,
    pub local_imports: bool,
    pub activity: bool,
    pub queue: bool,
}
impl Default for BackupContents {
    fn default() -> Self {
        Self {
            settings: true,
            saved_logins: true,
            playlists: true,
            favorites: true,
            local_imports: true,
            activity: true,
            queue: true,
        }
    }
}
impl BackupContents {
    pub fn intersect(self, present: Self) -> Self {
        Self {
            settings: self.settings && present.settings,
            saved_logins: self.saved_logins && present.saved_logins,
            playlists: self.playlists && present.playlists,
            favorites: self.favorites && present.favorites,
            local_imports: self.local_imports && present.local_imports,
            activity: self.activity && present.activity,
            queue: self.queue && present.queue,
        }
    }
    fn members(self) -> impl Iterator<Item = &'static str> {
        [
            ("settings.json", self.settings),
            ("saved-logins.json", self.saved_logins),
            ("playlist-order.jsonl", self.playlists),
            ("playlist-pins.json", self.playlists),
            ("smart.jsonl", self.playlists),
            ("user-state.jsonl", self.favorites),
            ("local-locators.jsonl", self.local_imports),
            ("activity.jsonl", self.activity),
            ("legacy-activity.jsonl", self.activity),
            ("queue.jsonl", self.queue),
            ("manifest.json", true),
        ]
        .into_iter()
        .filter_map(|(name, present)| present.then_some(name))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub version: u32,
    pub created_at: i64,
    pub playlist_count: u64,
    pub contents: BackupContents,
    pub schedule_id: Option<String>,
}
pub struct BackupOptions<'a> {
    pub contents: BackupContents,
    pub settings: Option<&'a [u8]>,
    pub saved_logins: Option<&'a [u8]>,
    pub playlist_pins: Option<&'a [u8]>,
    pub passphrase: Option<&'a str>,
    pub schedule_id: Option<&'a str>,
}
#[derive(Debug)]
pub struct StagedBackup {
    directory: tempfile::TempDir,
    pub manifest: BackupManifest,
}
impl StagedBackup {
    pub fn settings(&self) -> LibraryResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("settings.json"))?)
    }
    pub fn saved_logins(&self) -> LibraryResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("saved-logins.json"))?)
    }
    pub fn playlist_pins(&self) -> LibraryResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("playlist-pins.json"))?)
    }
    fn open(&self, name: &str) -> LibraryResult<BufReader<File>> {
        Ok(BufReader::new(File::open(
            self.directory.path().join(name),
        )?))
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
fn write_json_file(path: &Path, value: &[u8]) -> LibraryResult<()> {
    if value.len() as u64 > METADATA_LIMIT {
        return Err(invalid(
            "backup Settings or saved logins exceed the metadata limit",
        ));
    }
    let json: serde_json::Value = serde_json::from_slice(value)?;
    if !json.is_object() {
        return Err(invalid(
            "backup Settings and saved logins must be JSON objects",
        ));
    }
    let mut file = private_file(path)?;
    file.write_all(value)?;
    file.sync_all()?;
    Ok(())
}
impl Database {
    pub async fn write_backup(
        &self,
        output: impl Write,
        options: BackupOptions<'_>,
    ) -> LibraryResult<BackupManifest> {
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join("playlists"))?;
        let contents = options.contents;
        if contents.settings {
            write_json_file(
                &directory.path().join("settings.json"),
                options
                    .settings
                    .ok_or_else(|| invalid("selected Settings are missing"))?,
            )?;
        }
        if contents.saved_logins {
            write_json_file(
                &directory.path().join("saved-logins.json"),
                options
                    .saved_logins
                    .ok_or_else(|| invalid("selected saved logins are missing"))?,
            )?;
        }
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let mut ordinal = 0;
        if contents.playlists {
            write_json_file(
                &directory.path().join("playlist-pins.json"),
                options.playlist_pins.unwrap_or(b"{}"),
            )?;
            crate::playlists::export_playlist_order_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("playlist-order.jsonl"))?,
            )
            .await?;
            let mut cursor = -1;
            while let Some(key) = sqlx::query_scalar::<_, PlaylistKey>(
                "SELECT playlist_key FROM main.playlists WHERE playlist_key>?1 AND name IS NOT NULL ORDER BY playlist_key LIMIT 1")
                .bind(cursor).fetch_optional(&mut *transaction).await? {
                let path = directory.path().join(format!("playlists/{ordinal}.m3u8"));
                crate::m3u::export_playlist_m3u_on(&mut transaction, key, &path, private_file(&path)?).await?;
                cursor = key.raw();
                ordinal += 1;
            }
            crate::smart_playlists::export_smart_playlists_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("smart.jsonl"))?,
            )
            .await?;
        }
        if contents.favorites {
            crate::favorites::export_user_media_state_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("user-state.jsonl"))?,
            )
            .await?;
        }
        if contents.queue {
            crate::queue::export_queue_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("queue.jsonl"))?,
            )
            .await?;
        }
        if contents.activity {
            crate::activity::export_activity_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("activity.jsonl"))?,
                None,
            )
            .await?;
            crate::activity::export_legacy_activity_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("legacy-activity.jsonl"))?,
            )
            .await?;
        }
        if contents.local_imports {
            crate::local::export_local_locators_jsonl_on(
                &mut transaction,
                private_file(&directory.path().join("local-locators.jsonl"))?,
            )
            .await?;
        }
        transaction.commit().await?;
        drop(connection);
        let manifest = BackupManifest {
            version: 1,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            playlist_count: ordinal,
            contents,
            schedule_id: options.schedule_id.map(str::to_owned),
        };
        serde_json::to_writer(
            private_file(&directory.path().join("manifest.json"))?,
            &manifest,
        )?;
        if let Some(passphrase) = options.passphrase {
            if passphrase.is_empty() {
                return Err(invalid("backup passphrase cannot be empty"));
            }
            let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
                passphrase.to_owned(),
            ));
            let mut encrypted = encryptor
                .wrap_output(output)
                .map_err(|error| invalid(error.to_string()))?;
            write_archive(&directory, &manifest, &mut encrypted)?;
            encrypted.finish()?;
        } else {
            write_archive(&directory, &manifest, output)?;
        }
        Ok(manifest)
    }

    /// Replace selected groups in one transaction. Absent groups never clear live state.
    pub async fn restore_backup(
        &self,
        backup: &StagedBackup,
        contents: BackupContents,
    ) -> LibraryResult<BackupRestoreReport> {
        let contents = contents.intersect(backup.manifest.contents);
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
                backup.open("playlist-order.jsonl")?,
            )
            .await?;
            for ordinal in 0..backup.manifest.playlist_count {
                let name = format!("playlists/{ordinal}.m3u8");
                let path = backup.directory.path().join(&name);
                let imported = crate::m3u::import_playlist_m3u_on(
                    &mut transaction,
                    backup.open(&name)?,
                    &path,
                    |_| None,
                )
                .await?;
                report.playlist_entries += imported.imported;
                report.skipped_playlist_entries += imported.skipped;
            }
            report.smart_playlists = crate::smart_playlists::import_smart_playlists_jsonl_on(
                &mut transaction,
                backup.open("smart.jsonl")?,
            )
            .await?;
        }
        if contents.favorites {
            sqlx::raw_sql("DELETE FROM user_media_state;DELETE FROM favorite_outbox;")
                .execute(&mut *transaction)
                .await?;
            report.user_states = crate::favorites::import_user_media_state_jsonl_on(
                &mut transaction,
                backup.open("user-state.jsonl")?,
            )
            .await?;
        }
        if contents.queue {
            sqlx::raw_sql("DELETE FROM queue_occurrences;DELETE FROM queue_state;")
                .execute(&mut *transaction)
                .await?;
            crate::queue::import_queue_jsonl_on(&mut transaction, backup.open("queue.jsonl")?)
                .await?;
        }
        if contents.activity {
            sqlx::raw_sql("DELETE FROM listens;DELETE FROM legacy_activity;UPDATE tracks SET local_play_count=0 WHERE local_play_count<>0;")
                .execute(&mut *transaction)
                .await?;
            let activity = crate::activity::import_activity_jsonl_on(
                &mut transaction,
                backup.open("activity.jsonl")?,
            )
            .await?;
            report.listens = activity.accepted;
            report.skipped_listens = activity.skipped;
            crate::activity::import_legacy_activity_jsonl_on(
                &mut transaction,
                backup.open("legacy-activity.jsonl")?,
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
                    backup.open("local-locators.jsonl")?,
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(report)
    }
}
fn write_archive(
    directory: &tempfile::TempDir,
    manifest: &BackupManifest,
    output: impl Write,
) -> LibraryResult<()> {
    let compressed = flate2::write::GzEncoder::new(output, flate2::Compression::default());
    let mut archive = tar::Builder::new(compressed);
    for name in manifest.contents.members() {
        archive.append_path_with_name(directory.path().join(name), name)?;
    }
    for ordinal in 0..manifest.playlist_count {
        let name = format!("playlists/{ordinal}.m3u8");
        archive.append_path_with_name(directory.path().join(&name), &name)?;
    }
    archive.into_inner()?.finish()?;
    Ok(())
}

pub fn stage_backup(input: impl Read, passphrase: Option<&str>) -> LibraryResult<StagedBackup> {
    let mut input = BufReader::new(input);
    let mut prefix = [0; 22];
    input.read_exact(&mut prefix)?;
    let encrypted = prefix == *b"age-encryption.org/v1\n";
    let input = std::io::Cursor::new(prefix).chain(input);
    let mut tar_file = tempfile::tempfile()?;
    if encrypted {
        let passphrase =
            passphrase.ok_or_else(|| invalid("this backup requires its passphrase"))?;
        let decryptor = age::Decryptor::new(input).map_err(|error| invalid(error.to_string()))?;
        let identity =
            age::scrypt::Identity::new(age::secrecy::SecretString::from(passphrase.to_owned()));
        let decrypted = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|error| invalid(error.to_string()))?;
        io::copy(
            &mut flate2::read::MultiGzDecoder::new(decrypted),
            &mut tar_file,
        )?;
    } else {
        io::copy(&mut flate2::read::MultiGzDecoder::new(input), &mut tar_file)?;
    }
    let size = tar_file.metadata()?.len();
    if size < 1024 || size % 512 != 0 {
        return Err(invalid("incomplete backup tar framing"));
    }
    tar_file.seek(SeekFrom::End(-1024))?;
    let mut ending = [0; 1024];
    tar_file.read_exact(&mut ending)?;
    if ending.iter().any(|byte| *byte != 0) {
        return Err(invalid("backup tar end marker is missing"));
    }
    tar_file.rewind()?;
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join("playlists"))?;
    let mut archive = tar::Archive::new(tar_file);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid("backup contains an unsafe path"));
        }
        let Some(name) = path.to_str() else { continue };
        let known = BackupContents::default()
            .members()
            .any(|member| member == name)
            || playlist_member_number(name).is_some();
        if !known {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(invalid("backup data must be a regular file"));
        }
        if name.ends_with(".json") && entry.size() > METADATA_LIMIT {
            return Err(invalid("backup metadata exceeds its size limit"));
        }
        let mut file = private_file(&directory.path().join(&path))?;
        io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
    }
    let manifest: BackupManifest =
        serde_json::from_reader(File::open(directory.path().join("manifest.json"))?)?;
    if manifest.version != 1 {
        return Err(invalid("unsupported backup version"));
    }
    for name in manifest.contents.members() {
        if !directory.path().join(name).is_file() {
            return Err(invalid(format!("backup is missing {name}")));
        }
    }
    if manifest.contents.playlists {
        for ordinal in 0..manifest.playlist_count {
            if !directory
                .path()
                .join(format!("playlists/{ordinal}.m3u8"))
                .is_file()
            {
                return Err(invalid("backup playlist member order is incomplete"));
            }
        }
    }
    Ok(StagedBackup {
        directory,
        manifest,
    })
}
fn playlist_member_number(name: &str) -> Option<u64> {
    let number = name.strip_prefix("playlists/")?.strip_suffix(".m3u8")?;
    let parsed = number.parse::<u64>().ok()?;
    (parsed.to_string() == number).then_some(parsed)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackupFrequency {
    #[default]
    Off,
    Daily,
    Weekly,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackupSchedule {
    pub frequency: BackupFrequency,
    pub hour: u8,
    /// Monday is zero.
    pub weekday: u8,
    pub retention: usize,
    pub last_successful_at: Option<i64>,
    pub schedule_id: String,
}
impl Default for BackupSchedule {
    fn default() -> Self {
        Self {
            frequency: BackupFrequency::Off,
            hour: 2,
            weekday: 0,
            retention: 2,
            last_successful_at: None,
            schedule_id: String::new(),
        }
    }
}
impl BackupSchedule {
    pub fn validate(&self) -> LibraryResult<()> {
        if self.hour > 23 || self.weekday > 6 || self.retention == 0 {
            return Err(invalid(
                "backup schedule hour, weekday or retention is invalid",
            ));
        }
        if self.frequency != BackupFrequency::Off && self.schedule_id.is_empty() {
            return Err(invalid("enabled backup schedule has no identity"));
        }
        Ok(())
    }
    pub fn due_at(&self, now: i64) -> Option<i64> {
        self.due_at_in(now, &chrono::Local)
    }
    fn due_at_in<Tz: TimeZone>(&self, now: i64, zone: &Tz) -> Option<i64> {
        self.validate().ok()?;
        if self.frequency == BackupFrequency::Off {
            return None;
        }
        let now = zone.timestamp_opt(now, 0).single()?;
        for days in 0..=7 {
            let date = now.date_naive().checked_sub_days(chrono::Days::new(days))?;
            if self.frequency == BackupFrequency::Weekly
                && date.weekday().num_days_from_monday() != u32::from(self.weekday)
            {
                continue;
            }
            let clock = date.and_hms_opt(u32::from(self.hour), 0, 0)?;
            // At a spring-forward gap, run at the first valid local minute after the chosen hour.
            let due = (0..=180).find_map(|minutes| {
                zone.from_local_datetime(&(clock + chrono::Duration::minutes(minutes)))
                    .earliest()
            })?;
            if due > now {
                continue;
            }
            let due = due.timestamp();
            return self
                .last_successful_at
                .is_none_or(|last| last < due)
                .then_some(due);
        }
        None
    }
}
fn schedule_prefix(schedule_id: &str) -> String {
    format!(
        "rufin-scheduled-{}-",
        blake3::hash(schedule_id.as_bytes()).to_hex()
    )
}
pub fn backup_filename(schedule_id: Option<&str>, created_at: i64) -> String {
    let prefix = schedule_id
        .map(schedule_prefix)
        .unwrap_or_else(|| "rufin-backup-".into());
    format!("{prefix}{created_at}.rufin-backup")
}
pub fn scheduled_backup_timestamp(filename: &str, schedule_id: &str) -> Option<i64> {
    if schedule_id.is_empty() {
        return None;
    }
    let value = filename
        .strip_prefix(&schedule_prefix(schedule_id))?
        .strip_suffix(".rufin-backup")?;
    let timestamp = value.parse::<i64>().ok()?;
    (timestamp >= 0 && timestamp.to_string() == value).then_some(timestamp)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
    async fn archive(database: &Database, passphrase: Option<&str>) -> Vec<u8> {
        let mut bytes = Vec::new();
        database
            .write_backup(
                &mut bytes,
                BackupOptions {
                    contents: BackupContents::default(),
                    settings: Some(b"{\"sources\":[]}"),
                    saved_logins: Some(b"{}"),
                    playlist_pins: None,
                    passphrase,
                    schedule_id: Some("daily"),
                },
            )
            .await
            .unwrap();
        bytes
    }

    #[tokio::test]
    async fn backup_does_not_hold_the_route_lane_or_an_archive_time_reader() {
        struct PausedOutput {
            started: Option<tokio::sync::oneshot::Sender<()>>,
            resume: std::sync::mpsc::Receiver<()>,
        }
        impl Write for PausedOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(started) = self.started.take() {
                    let _ = started.send(());
                    self.resume.recv().map_err(io::Error::other)?;
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path(), "export").await;
        let (_permit, _foreground) = database
            .acquire_general(&crate::ReadCancellation::new())
            .await
            .unwrap();
        let export = database.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (resume, blocked) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Handle::current();
        let task = tokio::task::spawn_blocking(move || {
            runtime.block_on(export.write_backup(
                PausedOutput {
                    started: Some(started),
                    resume: blocked,
                },
                BackupOptions {
                    contents: BackupContents::default(),
                    settings: Some(b"{}"),
                    saved_logins: Some(b"{}"),
                    playlist_pins: None,
                    passphrase: None,
                    schedule_id: None,
                },
            ))
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        let mut reader =
            tokio::time::timeout(std::time::Duration::from_secs(1), database.acquire_reader())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&mut *reader)
                .await
                .unwrap(),
            1
        );
        resume.send(()).unwrap();
        task.await.unwrap().unwrap();
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
        let bytes = archive(&source, None).await;
        let staged = stage_backup(Cursor::new(&bytes), None).unwrap();
        assert_eq!(staged.settings().unwrap(), b"{\"sources\":[]}");
        assert_eq!(staged.saved_logins().unwrap(), b"{}");
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::query("CREATE TRIGGER reject_restore BEFORE INSERT ON user_media_state BEGIN SELECT RAISE(ABORT,'selected write failed'); END")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        assert!(
            target
                .restore_backup(&staged, BackupContents::default())
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
            .restore_backup(&staged, BackupContents::default())
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
                sources: vec![crate::QueueInstruction {
                    input: crate::QueueInput::Choices(
                        (0..250)
                            .map(|_| {
                                Some(crate::QueueChoice {
                                    origin: None,
                                    media_uri: "https://example.test/song".into(),
                                    fallback: None,
                                    provenance: crate::QueueProvenance::Manual,
                                })
                            })
                            .collect(),
                    ),
                    repeat: true,
                    seed: Some(7),
                }],
                pending: [crate::QueueCursor {
                    seed: Some(7),
                    ..Default::default()
                }]
                .into(),
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
        let bytes = archive(&source, None).await;
        let staged = stage_backup(Cursor::new(bytes), None).unwrap();
        assert_eq!(staged.manifest.playlist_count, 2);
        {
            let mut writer = target.writer().await.unwrap();
            sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(1,'catalog','Catalog','catalog',zeroblob(32),zeroblob(32)); INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,local_play_count) VALUES(1,'song','https://example.test/song','Song','song','','','song',100,7);").execute(writer.as_mut().unwrap()).await.unwrap();
        }
        target
            .restore_backup(&staged, BackupContents::default())
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
        let favorites = BackupContents {
            settings: false,
            saved_logins: false,
            playlists: false,
            favorites: true,
            local_imports: false,
            activity: false,
            queue: false,
        };
        let mut archive = Vec::new();
        source
            .write_backup(
                &mut archive,
                BackupOptions {
                    contents: favorites,
                    settings: None,
                    saved_logins: None,
                    playlist_pins: None,
                    passphrase: None,
                    schedule_id: None,
                },
            )
            .await
            .unwrap();
        let staged = stage_backup(Cursor::new(&archive), None).unwrap();
        assert_eq!(staged.manifest.contents, favorites);
        target
            .restore_backup(&staged, BackupContents::default())
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
        let mut archive = Vec::new();
        empty
            .write_backup(
                &mut archive,
                BackupOptions {
                    contents: favorites,
                    settings: None,
                    saved_logins: None,
                    playlist_pins: None,
                    passphrase: None,
                    schedule_id: None,
                },
            )
            .await
            .unwrap();
        let staged = stage_backup(Cursor::new(archive), None).unwrap();
        target.restore_backup(&staged, favorites).await.unwrap();
        let mut writer = target.writer().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_media_state")
                .fetch_one(writer.as_mut().unwrap())
                .await
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn age_authentication_and_truncated_gzip_fail_before_restore() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let bytes = archive(&source, Some("correct passphrase")).await;
        assert!(bytes.starts_with(b"age-encryption.org/v1\n"));
        assert!(stage_backup(Cursor::new(&bytes), None).is_err());
        assert!(stage_backup(Cursor::new(&bytes), Some("wrong")).is_err());
        assert!(stage_backup(Cursor::new(&bytes), Some("correct passphrase")).is_ok());
        let mut damaged = bytes;
        damaged.pop();
        assert!(stage_backup(Cursor::new(damaged), Some("correct passphrase")).is_err());
        let mut gzip = archive(&source, None).await;
        gzip.truncate(gzip.len() - 4);
        assert!(stage_backup(Cursor::new(gzip), None).is_err());
    }
    #[tokio::test]
    async fn unknown_members_are_ignored_but_duplicate_and_traversal_members_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let bytes = archive(&source, None).await;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(Cursor::new(bytes))
            .read_to_end(&mut raw)
            .unwrap();
        for name in ["../settings.json", "settings.json", "unrecognized.json"] {
            let mut original = tar::Archive::new(Cursor::new(&raw));
            let mut builder = tar::Builder::new(Vec::new());
            for entry in original.entries().unwrap() {
                let mut entry = entry.unwrap();
                let header = entry.header().clone();
                builder.append(&header, &mut entry).unwrap();
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o600);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, Cursor::new(b"{}")).unwrap();
            let modified = builder.into_inner().unwrap();
            let mut compressed =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            compressed.write_all(&modified).unwrap();
            assert_eq!(
                stage_backup(Cursor::new(compressed.finish().unwrap()), None).is_ok(),
                name == "unrecognized.json",
                "member {name}"
            );
        }
    }
    #[test]
    fn daily_and_weekly_schedules_track_normal_last_success() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 9, 5, 12, 0, 0)
            .unwrap()
            .timestamp();
        let mut schedule = BackupSchedule {
            frequency: BackupFrequency::Daily,
            schedule_id: "mine".into(),
            ..Default::default()
        };
        let due = schedule.due_at_in(now, &chrono::Utc).unwrap();
        assert_eq!(now - due, 10 * 3600);
        schedule.last_successful_at = Some(due);
        assert_eq!(schedule.due_at_in(now, &chrono::Utc), None);
        schedule.frequency = BackupFrequency::Weekly;
        schedule.weekday = 0;
        schedule.last_successful_at = None;
        assert_eq!(
            now - schedule.due_at_in(now, &chrono::Utc).unwrap(),
            5 * 86400 + 10 * 3600
        );
    }
}
