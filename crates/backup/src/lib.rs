//! Streaming semantic backups. Archive framing is staged before any live owner is changed.
use chrono::{Datelike, TimeZone};
use library::{BackupRestoreReport, Database, LibraryError, StateGroups};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const METADATA_LIMIT: u64 = 16 * 1024 * 1024;
fn invalid(message: impl Into<String>) -> BackupError {
    BackupError::InvalidRequest(message.into())
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
    fn state_groups(self) -> StateGroups {
        StateGroups {
            playlists: self.playlists,
            favorites: self.favorites,
            local_imports: self.local_imports,
            activity: self.activity,
            queue: self.queue,
        }
    }
    fn members(self) -> impl Iterator<Item = &'static str> {
        self.state_groups().members().chain(
            [
                ("settings.json", self.settings),
                ("saved-logins.json", self.saved_logins),
                ("playlist-pins.json", self.playlists),
                ("manifest.json", true),
            ]
            .into_iter()
            .filter_map(|(name, present)| present.then_some(name)),
        )
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
    pub fn settings(&self) -> BackupResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("settings.json"))?)
    }
    pub fn saved_logins(&self) -> BackupResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("saved-logins.json"))?)
    }
    pub fn playlist_pins(&self) -> BackupResult<Vec<u8>> {
        Ok(fs::read(self.directory.path().join("playlist-pins.json"))?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the Library request is invalid: {0}")]
    InvalidRequest(String),
}
pub type BackupResult<T> = Result<T, BackupError>;

fn private_file(path: &Path) -> BackupResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn write_json_file(path: &Path, value: &[u8]) -> BackupResult<()> {
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
pub async fn write_backup(
    database: &Database,
    output: impl Write,
    options: BackupOptions<'_>,
) -> BackupResult<BackupManifest> {
    let directory = tempfile::tempdir()?;
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
    if contents.playlists {
        write_json_file(
            &directory.path().join("playlist-pins.json"),
            options.playlist_pins.unwrap_or(b"{}"),
        )?;
    }
    let ordinal = database
        .export_state(directory.path(), contents.state_groups())
        .await?;
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

pub async fn restore_backup(
    database: &Database,
    backup: &StagedBackup,
    contents: BackupContents,
) -> BackupResult<BackupRestoreReport> {
    let contents = contents.intersect(backup.manifest.contents);
    Ok(database
        .restore_state(
            backup.directory.path(),
            contents.state_groups(),
            backup.manifest.playlist_count,
        )
        .await?)
}
fn write_archive(
    directory: &tempfile::TempDir,
    manifest: &BackupManifest,
    output: impl Write,
) -> BackupResult<()> {
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

pub fn stage_backup(input: impl Read, passphrase: Option<&str>) -> BackupResult<StagedBackup> {
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
    pub fn validate(&self) -> BackupResult<()> {
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
    async fn archive(database: &Database, passphrase: Option<&str>) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_backup(
            database,
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
    #[tokio::test]
    async fn archive_output_does_not_hold_a_store_reader() {
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
        let export = database.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (resume, blocked) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Handle::current();
        let task = tokio::task::spawn_blocking(move || {
            runtime.block_on(write_backup(
                &export,
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
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            database.user_media_state(
                "https://example.test/song",
                &library::ReadCancellation::new(),
            ),
        )
        .await;
        resume.send(()).unwrap();
        assert_eq!(read.unwrap().unwrap(), None);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn restore_intersects_requested_and_present_groups_before_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let empty = database(directory.path(), "empty").await;
        let target = database(directory.path(), "target").await;
        let uri = "https://example.test/song";
        target
            .set_rating(&library::FavoriteTarget::Track(uri.into()), Some(2))
            .await
            .unwrap();
        let mut contents = BackupContents {
            settings: false,
            saved_logins: false,
            playlists: false,
            favorites: false,
            local_imports: false,
            activity: false,
            queue: false,
        };
        for present in [false, true] {
            contents.favorites = present;
            let mut bytes = Vec::new();
            write_backup(
                &empty,
                &mut bytes,
                BackupOptions {
                    contents,
                    settings: None,
                    saved_logins: None,
                    playlist_pins: None,
                    passphrase: None,
                    schedule_id: None,
                },
            )
            .await
            .unwrap();
            let staged = stage_backup(Cursor::new(bytes), None).unwrap();
            let mut requested = BackupContents::default();
            requested.favorites = !present;
            restore_backup(&target, &staged, requested).await.unwrap();
            assert_eq!(
                target
                    .user_media_state(uri, &library::ReadCancellation::new())
                    .await
                    .unwrap(),
                Some((None, Some(2)))
            );
            if present {
                restore_backup(&target, &staged, BackupContents::default())
                    .await
                    .unwrap();
                assert_eq!(
                    target
                        .user_media_state(uri, &library::ReadCancellation::new())
                        .await
                        .unwrap(),
                    None
                );
            }
        }
    }

    #[tokio::test]
    async fn archive_roundtrip_restores_selected_data_and_application_members() {
        let directory = tempfile::tempdir().unwrap();
        let source = database(directory.path(), "source").await;
        let target = database(directory.path(), "target").await;
        let uri = "https://example.test/song";
        let favorite = library::FavoriteTarget::Track(uri.into());
        source.set_rating(&favorite, Some(7)).await.unwrap();
        target.set_rating(&favorite, Some(2)).await.unwrap();
        let bytes = archive(&source, None).await;
        let staged = stage_backup(Cursor::new(bytes), None).unwrap();
        assert_eq!(staged.settings().unwrap(), br#"{"sources":[]}"#);
        assert_eq!(staged.saved_logins().unwrap(), b"{}");
        assert_eq!(staged.playlist_pins().unwrap(), b"{}");
        let report = restore_backup(&target, &staged, BackupContents::default())
            .await
            .unwrap();
        assert_eq!(report.user_states, 1);
        assert_eq!(
            target
                .user_media_state(uri, &library::ReadCancellation::new())
                .await
                .unwrap(),
            Some((None, Some(7)))
        );
    }
}
