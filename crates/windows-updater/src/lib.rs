#[cfg(any(target_os = "windows", test))]
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
#[cfg(target_os = "windows")]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::io::{BufRead, BufReader};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use std::process::{Command, Stdio};
#[cfg(target_os = "windows")]
use std::sync::mpsc;

use app_identity::PROJECT_NAME;
#[cfg(target_os = "windows")]
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "windows", test))]
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(any(target_os = "windows", test))]
const CHANNEL_FILE: &str = "update-channel";
#[cfg(any(target_os = "windows", test))]
const HELPER_FILE: &str = "rufin-update-helper.exe";
#[cfg(any(target_os = "windows", test))]
const HELPER_SENTINEL: &str = "rufin-update-helper.complete";
const UPDATE_CACHE_DIR: &str = "windows-update";
const RESULT_FILE: &str = "result.json";
#[cfg(target_os = "windows")]
const READY_LINE: &str = "READY";
#[cfg(target_os = "windows")]
const READY_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(target_os = "windows")]
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESULT_BYTES: u64 = 16 * 1024;
#[cfg(any(target_os = "windows", test))]
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FAILURE_MESSAGE_BYTES: usize = 1024;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
const PENDING_FAILURE_MESSAGE: &str = "The update did not finish after Rufin closed.";

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelaunchStatus {
    Ready,
    Present,
    Visible,
}

#[cfg(any(target_os = "windows", test))]
impl RelaunchStatus {
    fn line(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::Present => "PRESENT",
            Self::Visible => "VISIBLE",
        }
    }

    fn parse(line: &str) -> Option<Self> {
        Some(match line.trim_end_matches(['\r', '\n']) {
            "READY" => Self::Ready,
            "PRESENT" => Self::Present,
            "VISIBLE" => Self::Visible,
            _ => return None,
        })
    }
}

#[cfg(target_os = "windows")]
fn allow_set_foreground_window(process_id: u32) -> bool {
    winsafe::AllowSetForegroundWindow(Some(process_id)).is_ok()
}

/// Waits for the updater helper to grant an updater-driven restart permission
/// to present its window.
#[cfg(target_os = "windows")]
pub fn wait_for_updated_restart() -> Result<(), String> {
    io::stdout()
        .write_all(format!("{}\n", RelaunchStatus::Ready.line()).as_bytes())
        .and_then(|()| io::stdout().flush())
        .map_err(|error| format!("could not report Rufin restart readiness: {error}"))?;
    let (sender, receiver) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = io::stdin()
            .lock()
            .take(64)
            .read_line(&mut line)
            .map(|_| line);
        let _ = sender.send(result);
    });
    let permission = receiver
        .recv_timeout(RELAUNCH_TIMEOUT)
        .map_err(|_| "the update helper did not grant window presentation".to_owned())?
        .map_err(|error| format!("could not read the update helper response: {error}"))?;
    if RelaunchStatus::parse(&permission) != Some(RelaunchStatus::Present) {
        return Err("the update helper sent an invalid presentation response".to_owned());
    }
    Ok(())
}

/// Reports that the updater-driven Rufin window reached GTK's mapped state.
#[cfg(target_os = "windows")]
pub fn report_updated_restart_visible() -> Result<(), String> {
    io::stdout()
        .write_all(format!("{}\n", RelaunchStatus::Visible.line()).as_bytes())
        .and_then(|()| io::stdout().flush())
        .map_err(|error| format!("could not report the reopened Rufin window: {error}"))
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UpdateChannel {
    Direct,
    Scoop,
    Winget,
}

#[cfg(any(target_os = "windows", test))]
impl UpdateChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Scoop => "scoop",
            Self::Winget => "winget",
        }
    }
}

#[cfg(any(target_os = "windows", test))]
fn channel_supports_automatic_updates(channel: UpdateChannel) -> bool {
    matches!(channel, UpdateChannel::Direct | UpdateChannel::Scoop)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
enum UpdateUnavailable {
    #[cfg(not(target_os = "windows"))]
    #[error("updates are only available in the installed Windows app")]
    NotWindows,
    #[cfg(any(target_os = "windows", test))]
    #[error("this copy of Rufin is not installed in the Windows package layout")]
    NotInstalled,
    #[cfg(any(target_os = "windows", test))]
    #[error("this Rufin installation has no update channel")]
    MissingChannel,
    #[cfg(any(target_os = "windows", test))]
    #[error("this Rufin installation has an unknown update channel")]
    UnknownChannel,
    #[cfg(any(target_os = "windows", test))]
    #[error("the installed update helper is unavailable")]
    MissingHelper,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
pub enum PreviousUpdateResult {
    Installed { version: String },
    Failed { version: String, message: String },
}

impl PreviousUpdateResult {
    #[must_use]
    pub fn version(&self) -> &str {
        match self {
            Self::Installed { version } | Self::Failed { version, .. } => version,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstalledUpdater {
    #[cfg(target_os = "windows")]
    support: InstalledSupport,
    cache_dir: PathBuf,
}

impl InstalledUpdater {
    /// Detects an installed, channel-owned updater. Missing or unknown channel
    /// markers deliberately return `None`, so development and copied builds do
    /// not gain an updater by inference.
    pub fn detect(cache_dir: PathBuf) -> Result<Option<Self>, String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = cache_dir;
            Ok(None)
        }

        #[cfg(target_os = "windows")]
        {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let support = directories::BaseDirs::new()
                .map(|dirs| dirs.data_local_dir().join("Programs").join("Rufin"))
                .and_then(|install_root| {
                    detect_support_at(&executable, env!("CARGO_PKG_VERSION"), &install_root).ok()
                });
            if let Some(support) = support.as_ref() {
                cleanup_old_helpers(&support.updater_root, env!("CARGO_PKG_VERSION"));
            }
            Ok(support.map(|support| Self { support, cache_dir }))
        }
    }

    /// Starts this installation channel's fixed update command and returns
    /// only after the helper reports that it is waiting for Rufin to exit.
    pub fn install(&self, version: &str) -> Result<(), String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = &self.cache_dir;
            let _ = version;
            Err(UpdateUnavailable::NotWindows.to_string())
        }

        #[cfg(target_os = "windows")]
        {
            start_installed_update(&self.support, version, &self.cache_dir)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    }

    /// Automatic launch updates are available when the installed channel can
    /// complete its update without user interaction.
    #[must_use]
    pub fn supports_automatic_updates(&self) -> bool {
        #[cfg(not(target_os = "windows"))]
        {
            false
        }

        #[cfg(target_os = "windows")]
        {
            channel_supports_automatic_updates(self.support.channel)
        }
    }
}

#[derive(Debug, Error)]
enum UpdateError {
    #[error("invalid release version: {0}")]
    InvalidVersion(String),
    #[error("could not prepare the update: {0}")]
    Io(#[from] io::Error),
    #[cfg(target_os = "windows")]
    #[error("could not download the Windows installer: {0}")]
    Download(#[from] reqwest::Error),
    #[cfg(target_os = "windows")]
    #[error("the downloaded Windows installer did not match its published SHA-256 digest")]
    DigestMismatch,
    #[cfg(any(target_os = "windows", test))]
    #[error("the downloaded Windows installer was empty")]
    EmptyInstaller,
    #[cfg(any(target_os = "windows", test))]
    #[error("the downloaded Windows installer was larger than 512 MiB")]
    InstallerTooLarge,
    #[cfg(target_os = "windows")]
    #[error("the update helper did not become ready")]
    HelperNotReady,
    #[cfg(target_os = "windows")]
    #[error("Windows did not permit the updater to transfer foreground activation")]
    ForegroundTransfer,
    #[error("the previous update result is invalid")]
    InvalidResult,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone)]
struct InstalledSupport {
    channel: UpdateChannel,
    helper_dir: PathBuf,
    updater_root: PathBuf,
    relaunch: PathBuf,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    assets: Vec<GitHubAsset>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug)]
struct HelperArgs {
    parent_pid: u32,
    channel: UpdateChannel,
    target_version: String,
    result_file: PathBuf,
    relaunch: PathBuf,
    installer: Option<PathBuf>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<OsString>,
}

#[cfg(target_os = "windows")]
fn start_installed_update(
    support: &InstalledSupport,
    target_version: &str,
    cache_dir: &Path,
) -> Result<(), UpdateError> {
    let target_version = normalize_version(target_version)?;
    let update_root = update_root(cache_dir);
    fs::create_dir_all(&update_root)?;
    remove_file_if_present(&result_path(cache_dir))?;

    let installer = if support.channel == UpdateChannel::Direct {
        Some(download_installer(&target_version, &update_root)?)
    } else {
        None
    };
    let helper = support.helper_dir.join(HELPER_FILE);
    let mut command = Command::new(&helper);
    command
        .current_dir(&support.helper_dir)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--channel")
        .arg(support.channel.as_str())
        .arg("--target-version")
        .arg(&target_version)
        .arg("--result-file")
        .arg(result_path(cache_dir))
        .arg("--relaunch")
        .arg(&support.relaunch)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(installer) = installer {
        command.arg("--installer").arg(installer);
    }

    let mut child = command.spawn()?;
    if !allow_set_foreground_window(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = remove_file_if_present(&result_path(cache_dir));
        return Err(UpdateError::ForegroundTransfer);
    }
    let stdout = child.stdout.take().ok_or(UpdateError::HelperNotReady)?;
    let (sender, receiver) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout)
            .take(64)
            .read_line(&mut line)
            .map(|_| line);
        let _ = sender.send(result);
    });
    let ready = receiver
        .recv_timeout(READY_TIMEOUT)
        .ok()
        .and_then(Result::ok);
    if ready.as_deref().map(str::trim_end) != Some(READY_LINE) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = remove_file_if_present(&result_path(cache_dir));
        return Err(UpdateError::HelperNotReady);
    }

    Ok(())
}

/// Reads and removes the bounded result left by the previous helper run.
fn take_previous_result(cache_dir: &Path) -> Result<Option<PreviousUpdateResult>, UpdateError> {
    let path = result_path(cache_dir);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > MAX_RESULT_BYTES {
        remove_file_if_present(&path)?;
        return Err(UpdateError::InvalidResult);
    }
    let mut bytes = Vec::new();
    file.take(MAX_RESULT_BYTES + 1).read_to_end(&mut bytes)?;
    remove_file_if_present(&path)?;
    if bytes.len() as u64 > MAX_RESULT_BYTES {
        return Err(UpdateError::InvalidResult);
    }
    let result: PreviousUpdateResult =
        serde_json::from_slice(&bytes).map_err(|_| UpdateError::InvalidResult)?;
    if normalize_version(result.version()).ok().as_deref() != Some(result.version())
        || matches!(
            &result,
            PreviousUpdateResult::Failed { message, .. }
                if message.is_empty() || message.len() > MAX_FAILURE_MESSAGE_BYTES
        )
    {
        return Err(UpdateError::InvalidResult);
    }
    Ok(Some(result))
}

/// Convenience entry point used during application startup, before a channel
/// handle has necessarily been detected.
pub fn take_previous_update_result() -> Result<Option<PreviousUpdateResult>, String> {
    let cache_dir = directories::ProjectDirs::from("io.github", "screwys", PROJECT_NAME)
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .ok_or_else(|| {
            UpdateError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "Rufin's cache directory is unavailable",
            ))
        })
        .map_err(|error| error.to_string())?;
    take_previous_result(&cache_dir).map_err(|error| error.to_string())
}

/// Removes only complete old installer-owned helper directories. Failures are
/// ignored because a just-finished helper may still have its executable open.
#[cfg(any(target_os = "windows", test))]
fn cleanup_old_helpers(updater_root: &Path, current_version: &str) {
    let Ok(entries) = fs::read_dir(updater_root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if version == current_version {
            continue;
        }
        if let Some(files) = owned_old_helper_files(&entry.path(), &version) {
            for file in files {
                if fs::remove_file(file).is_err() {
                    break;
                }
            }
            let _ = fs::remove_dir(entry.path());
        }
    }
}

pub fn run_helper() -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("rufin-update-helper is only available on Windows".to_owned())
    }

    #[cfg(target_os = "windows")]
    {
        run_helper_windows()
    }
}

fn update_root(cache_dir: &Path) -> PathBuf {
    cache_dir.join(UPDATE_CACHE_DIR)
}

fn result_path(cache_dir: &Path) -> PathBuf {
    update_root(cache_dir).join(RESULT_FILE)
}

fn normalize_version(version: &str) -> Result<String, UpdateError> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let safe = !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        && version.as_bytes()[0].is_ascii_digit()
        && !version.contains("..");
    if safe {
        Ok(version.to_owned())
    } else {
        Err(UpdateError::InvalidVersion(version.to_owned()))
    }
}

#[cfg(any(target_os = "windows", test))]
fn sentinel_contents(version: &str) -> String {
    format!("rufin-update-helper:{version}\n")
}

#[cfg(any(target_os = "windows", test))]
fn detect_support_at(
    executable: &Path,
    installed_version: &str,
    expected_install_root: &Path,
) -> Result<InstalledSupport, UpdateUnavailable> {
    if executable.file_name() != Some(OsStr::new("rufin.exe"))
        || executable.parent().and_then(Path::file_name) != Some(OsStr::new("bin"))
    {
        return Err(UpdateUnavailable::NotInstalled);
    }
    let install_root = executable
        .parent()
        .and_then(Path::parent)
        .ok_or(UpdateUnavailable::NotInstalled)?;
    let canonical_install_root = fs::canonicalize(install_root).ok();
    let canonical_expected_root = fs::canonicalize(expected_install_root).ok();
    if !canonical_install_root
        .zip(canonical_expected_root)
        .is_some_and(|(install_root, expected_root)| install_root == expected_root)
    {
        return Err(UpdateUnavailable::NotInstalled);
    }
    if !regular_owned_file(&install_root.join("Uninstall.exe")) {
        return Err(UpdateUnavailable::NotInstalled);
    }
    let channel = read_channel(&install_root.join(CHANNEL_FILE))?;
    let updater_root = install_root.join("updater");
    let helper_dir = updater_root.join(installed_version);
    let helper = helper_dir.join(HELPER_FILE);
    let sentinel = helper_dir.join(HELPER_SENTINEL);
    if !regular_owned_file(&helper)
        || !regular_owned_file(&sentinel)
        || fs::read_to_string(sentinel).ok().as_deref()
            != Some(sentinel_contents(installed_version).as_str())
    {
        return Err(UpdateUnavailable::MissingHelper);
    }
    Ok(InstalledSupport {
        channel,
        helper_dir,
        updater_root,
        relaunch: executable.to_path_buf(),
    })
}

#[cfg(any(target_os = "windows", test))]
fn read_channel(path: &Path) -> Result<UpdateChannel, UpdateUnavailable> {
    match fs::symlink_metadata(path) {
        Ok(_) if regular_owned_file(path) => {}
        Ok(_) => return Err(UpdateUnavailable::UnknownChannel),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(UpdateUnavailable::MissingChannel);
        }
        Err(_) => return Err(UpdateUnavailable::UnknownChannel),
    }
    let value = match fs::read_to_string(path) {
        Ok(value) if value.len() <= 32 => value,
        Ok(_) => return Err(UpdateUnavailable::UnknownChannel),
        Err(_) => return Err(UpdateUnavailable::UnknownChannel),
    };
    match value.trim() {
        "direct" => Ok(UpdateChannel::Direct),
        "scoop" => Ok(UpdateChannel::Scoop),
        "winget" => Ok(UpdateChannel::Winget),
        _ => Err(UpdateUnavailable::UnknownChannel),
    }
}

#[cfg(any(target_os = "windows", test))]
fn regular_owned_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && !metadata_is_reparse_point(&metadata)
    })
}

#[cfg(any(target_os = "windows", test))]
fn regular_owned_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_dir()
            && !metadata.file_type().is_symlink()
            && !metadata_is_reparse_point(&metadata)
    })
}

#[cfg(any(target_os = "windows", test))]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = metadata;
        false
    }
}

#[cfg(any(target_os = "windows", test))]
fn owned_old_helper_files(path: &Path, version: &str) -> Option<Vec<PathBuf>> {
    if !regular_owned_directory(path) || normalize_version(version).ok().as_deref() != Some(version)
    {
        return None;
    }

    let mut helper = None;
    let mut sentinel = None;
    let mut libraries = Vec::new();
    for entry in fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        let entry_path = entry.path();
        if !regular_owned_file(&entry_path) {
            return None;
        }
        let name = entry.file_name();
        if name == OsStr::new(HELPER_FILE) {
            helper = Some(entry_path);
        } else if name == OsStr::new(HELPER_SENTINEL) {
            sentinel = Some(entry_path);
        } else if name
            .to_str()
            .is_some_and(|name| name.to_ascii_lowercase().ends_with(".dll"))
        {
            libraries.push(entry_path);
        } else {
            return None;
        }
    }
    let helper = helper?;
    let sentinel = sentinel?;
    if fs::read_to_string(&sentinel).ok().as_deref() != Some(sentinel_contents(version).as_str()) {
        return None;
    }
    libraries.sort();
    let mut files = Vec::with_capacity(libraries.len() + 2);
    files.extend(libraries);
    files.push(helper);
    files.push(sentinel);
    Some(files)
}

#[cfg(any(target_os = "windows", test))]
fn release_asset_url(version: &str) -> String {
    format!(
        "https://github.com/screwys/Rufin/releases/download/v{version}/Rufin-{version}-setup.exe"
    )
}

#[cfg(target_os = "windows")]
fn release_api_url(version: &str) -> String {
    format!("https://api.github.com/repos/screwys/Rufin/releases/tags/v{version}")
}

#[cfg(any(target_os = "windows", test))]
fn installer_asset_name(version: &str) -> String {
    format!("Rufin-{version}-setup.exe")
}

#[cfg(any(target_os = "windows", test))]
fn optional_release_digest(release: &GitHubRelease, version: &str) -> Option<String> {
    let expected_name = installer_asset_name(version);
    let expected_url = release_asset_url(version);
    release
        .assets
        .iter()
        .find(|asset| asset.name == expected_name && asset.browser_download_url == expected_url)
        .and_then(|asset| asset.digest.as_deref())
        .and_then(parse_sha256)
}

#[cfg(any(target_os = "windows", test))]
fn parse_sha256(value: &str) -> Option<String> {
    let digest = value.strip_prefix("sha256:")?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

#[cfg(any(target_os = "windows", test))]
fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(any(target_os = "windows", test))]
fn validate_installer_size(size: u64) -> Result<(), UpdateError> {
    match size {
        0 => Err(UpdateError::EmptyInstaller),
        size if size > MAX_INSTALLER_BYTES => Err(UpdateError::InstallerTooLarge),
        _ => Ok(()),
    }
}

#[cfg(target_os = "windows")]
fn download_installer(version: &str, update_root: &Path) -> Result<PathBuf, UpdateError> {
    let client = Client::builder()
        .user_agent(format!("Rufin/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(5 * 60))
        .build()?;
    let expected_digest = client
        .get(release_api_url(version))
        .timeout(Duration::from_secs(10))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::json::<GitHubRelease>)
        .ok()
        .and_then(|release| optional_release_digest(&release, version));

    let downloads = update_root.join("downloads").join(version);
    fs::create_dir_all(&downloads)?;
    let installer = downloads.join(installer_asset_name(version));
    let partial = installer.with_extension("exe.part");
    remove_file_if_present(&partial)?;

    let download_result = (|| {
        let mut response = client
            .get(release_asset_url(version))
            .send()?
            .error_for_status()?;
        if let Some(length) = response.content_length() {
            validate_installer_size(length)?;
        }
        let mut output = File::create(&partial)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut downloaded = 0_u64;
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .ok_or(UpdateError::InstallerTooLarge)?;
            if downloaded > MAX_INSTALLER_BYTES {
                return Err(UpdateError::InstallerTooLarge);
            }
            output.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
        }
        validate_installer_size(downloaded)?;
        output.sync_all()?;
        drop(output);
        if expected_digest.as_deref().is_some_and(|expected| {
            let digest = hasher.finalize();
            lowercase_hex(&digest) != expected
        }) {
            return Err(UpdateError::DigestMismatch);
        }
        remove_file_if_present(&installer)?;
        fs::rename(&partial, &installer)?;
        Ok(installer.clone())
    })();
    if download_result.is_err() {
        let _ = fs::remove_file(partial);
    }
    download_result
}

fn remove_file_if_present(path: &Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "windows", test))]
fn cleanup_direct_installer(installer: Option<&Path>, result_file: &Path, target_version: &str) {
    let Some(installer) = installer else {
        return;
    };
    let Some(update_root) = result_file.parent() else {
        return;
    };
    let version_dir = update_root.join("downloads").join(target_version);
    let expected_installer = version_dir.join(installer_asset_name(target_version));
    if installer != expected_installer || !regular_owned_directory(&version_dir) {
        return;
    }
    match fs::symlink_metadata(installer) {
        Ok(_) if regular_owned_file(installer) => {
            let _ = fs::remove_file(installer);
        }
        Ok(_) => return,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return,
    }
    let _ = fs::remove_dir(version_dir);
}

#[cfg(any(target_os = "windows", test))]
fn command_plan(args: &HelperArgs) -> Result<Vec<CommandSpec>, String> {
    match args.channel {
        UpdateChannel::Direct => {
            let installer = args
                .installer
                .as_ref()
                .ok_or_else(|| "the direct update has no installer".to_owned())?;
            Ok(vec![CommandSpec {
                program: installer.clone(),
                args: vec![OsString::from("/S"), OsString::from("/RUFINCHANNEL=direct")],
            }])
        }
        UpdateChannel::Scoop => Ok(vec![
            CommandSpec {
                program: PathBuf::from("scoop.cmd"),
                args: vec![OsString::from("update")],
            },
            CommandSpec {
                program: PathBuf::from("scoop.cmd"),
                args: vec![OsString::from("update"), OsString::from("rufin")],
            },
        ]),
        UpdateChannel::Winget => Ok(vec![CommandSpec {
            program: PathBuf::from("winget.exe"),
            args: [
                "upgrade",
                "--id",
                "screwys.Rufin",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--disable-interactivity",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        }]),
    }
}

#[cfg(any(target_os = "windows", test))]
fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("duplicate {name}"))
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "windows", test))]
fn absolute_helper_path(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        path.is_absolute()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let value = path.to_string_lossy();
        let bytes = value.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
            || value.starts_with("\\\\")
    }
}

#[cfg(any(target_os = "windows", test))]
fn parse_helper_args<I>(args: I) -> Result<HelperArgs, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    let mut parent_pid = None;
    let mut channel = None;
    let mut target_version = None;
    let mut result_file = None;
    let mut relaunch = None;
    let mut installer = None;
    while let Some(option) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {}", option.to_string_lossy()))?;
        match option.to_str() {
            Some("--parent-pid") => {
                let parsed = value
                    .to_string_lossy()
                    .parse::<u32>()
                    .map_err(|_| "invalid parent process id".to_owned())?;
                if parsed == 0 {
                    return Err("invalid parent process id".to_owned());
                }
                set_once(&mut parent_pid, parsed, "parent process id")?;
            }
            Some("--channel") => {
                let parsed = match value.to_str() {
                    Some("direct") => UpdateChannel::Direct,
                    Some("scoop") => UpdateChannel::Scoop,
                    Some("winget") => UpdateChannel::Winget,
                    _ => return Err("invalid update channel".to_owned()),
                };
                set_once(&mut channel, parsed, "update channel")?;
            }
            Some("--target-version") => {
                let parsed = normalize_version(&value.to_string_lossy())
                    .map_err(|error| error.to_string())?;
                set_once(&mut target_version, parsed, "target version")?;
            }
            Some("--result-file") => {
                set_once(&mut result_file, PathBuf::from(value), "result file")?;
            }
            Some("--relaunch") => {
                set_once(&mut relaunch, PathBuf::from(value), "relaunch executable")?;
            }
            Some("--installer") => {
                set_once(&mut installer, PathBuf::from(value), "installer")?;
            }
            _ => {
                return Err(format!(
                    "unknown helper option: {}",
                    option.to_string_lossy()
                ));
            }
        }
    }
    let parsed = HelperArgs {
        parent_pid: parent_pid.ok_or_else(|| "missing parent process id".to_owned())?,
        channel: channel.ok_or_else(|| "missing update channel".to_owned())?,
        target_version: target_version.ok_or_else(|| "missing target version".to_owned())?,
        result_file: result_file.ok_or_else(|| "missing result file".to_owned())?,
        relaunch: relaunch.ok_or_else(|| "missing relaunch executable".to_owned())?,
        installer,
    };
    if !absolute_helper_path(&parsed.result_file) {
        return Err("the result file path must be absolute".to_owned());
    }
    if !absolute_helper_path(&parsed.relaunch) {
        return Err("the relaunch executable path must be absolute".to_owned());
    }
    match parsed.channel {
        UpdateChannel::Direct => {
            if !parsed
                .installer
                .as_deref()
                .is_some_and(absolute_helper_path)
            {
                return Err("the direct installer path must be absolute".to_owned());
            }
        }
        UpdateChannel::Scoop | UpdateChannel::Winget if parsed.installer.is_some() => {
            return Err("package-manager updates cannot include a direct installer".to_owned());
        }
        UpdateChannel::Scoop | UpdateChannel::Winget => {}
    }
    let _ = command_plan(&parsed)?;
    Ok(parsed)
}

#[cfg(any(target_os = "windows", test))]
fn bounded_message(message: &str) -> String {
    if message.len() <= MAX_FAILURE_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut bounded = String::with_capacity(MAX_FAILURE_MESSAGE_BYTES);
    for character in message.chars() {
        if bounded.len() + character.len_utf8() > MAX_FAILURE_MESSAGE_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

#[cfg(any(target_os = "windows", test))]
fn write_result(path: &Path, result: &PreviousUpdateResult) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "the update result has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(result).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RESULT_BYTES {
        return Err("the update result is too large".to_owned());
    }
    let partial = path.with_extension("json.part");
    fs::write(&partial, bytes).map_err(|error| error.to_string())?;
    let _ = fs::remove_file(path);
    fs::rename(partial, path).map_err(|error| error.to_string())
}

#[cfg(any(target_os = "windows", test))]
fn write_installed(path: &Path, version: &str) -> Result<(), String> {
    write_result(
        path,
        &PreviousUpdateResult::Installed {
            version: version.to_owned(),
        },
    )
}

#[cfg(any(target_os = "windows", test))]
fn write_failure(path: &Path, version: &str, message: &str) -> Result<(), String> {
    write_result(
        path,
        &PreviousUpdateResult::Failed {
            version: version.to_owned(),
            message: bounded_message(message),
        },
    )
}

#[cfg(target_os = "windows")]
fn relaunch_command(args: &HelperArgs) -> Command {
    let mut relaunch = Command::new(&args.relaunch);
    if let Some(install_root) = args.relaunch.parent().and_then(Path::parent) {
        relaunch.current_dir(install_root);
    }
    relaunch
}

#[cfg(target_os = "windows")]
fn reopen_after_failed_update(args: &HelperArgs) -> Result<(), String> {
    relaunch_command(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Rufin could not be reopened after the update: {error}"))
}

#[cfg(target_os = "windows")]
fn receive_relaunch_line(
    receiver: &mpsc::Receiver<io::Result<String>>,
    expected: RelaunchStatus,
) -> Result<(), String> {
    let line = receiver
        .recv_timeout(RELAUNCH_TIMEOUT)
        .map_err(|_| {
            format!(
                "Rufin did not report {} before the restart deadline",
                expected.line()
            )
        })?
        .map_err(|error| format!("could not read Rufin restart status: {error}"))?;
    if RelaunchStatus::parse(&line) != Some(expected) {
        return Err(format!("Rufin reported an invalid restart status: {line}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn reopen_after_successful_update(args: &HelperArgs) -> Result<(), String> {
    let mut child = relaunch_command(args)
        .arg("--updated-restart")
        .arg(&args.target_version)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("Rufin could not be reopened after the update: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "the reopened Rufin process has no readiness channel".to_owned())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "the reopened Rufin process has no presentation channel".to_owned())?;
    let (sender, receiver) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(2) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| {
        receive_relaunch_line(&receiver, RelaunchStatus::Ready)?;
        if !allow_set_foreground_window(child.id()) {
            return Err("Windows did not permit Rufin to take foreground activation".to_string());
        }
        stdin
            .write_all(format!("{}\n", RelaunchStatus::Present.line()).as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("could not permit Rufin window presentation: {error}"))?;
        receive_relaunch_line(&receiver, RelaunchStatus::Visible)
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(target_os = "windows")]
fn run_helper_windows() -> Result<(), String> {
    let raw_args = std::env::args_os().collect::<Vec<_>>();
    if raw_args.get(1).and_then(|arg| arg.to_str()) == Some("--self-check") {
        if raw_args.len() != 2 {
            return Err("self-check does not accept extra arguments".to_owned());
        }
        let helper = std::env::current_exe().map_err(|error| error.to_string())?;
        let helper_dir = helper
            .parent()
            .ok_or_else(|| "the update helper has no parent directory".to_owned())?;
        if fs::read_to_string(helper_dir.join(HELPER_SENTINEL))
            .ok()
            .as_deref()
            != Some(sentinel_contents(env!("CARGO_PKG_VERSION")).as_str())
        {
            return Err("the update helper sentinel is missing".to_owned());
        }
        return Ok(());
    }
    let args = parse_helper_args(raw_args)?;
    let parent =
        winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::SYNCHRONIZE, false, args.parent_pid)
            .map_err(|error| format!("could not open the Rufin process: {error}"))?;

    write_failure(
        &args.result_file,
        &args.target_version,
        PENDING_FAILURE_MESSAGE,
    )?;
    io::stdout()
        .write_all(format!("{READY_LINE}\n").as_bytes())
        .and_then(|()| io::stdout().flush())
        .map_err(|error| format!("could not report helper readiness: {error}"))?;
    let wait = parent
        .WaitForSingleObject(None)
        .map_err(|error| format!("could not wait for Rufin to exit: {error}"))?;
    if wait != winsafe::co::WAIT::OBJECT_0 {
        return Err("waiting for Rufin returned an unexpected result".to_owned());
    }

    let mut failure = None;
    for spec in command_plan(&args)? {
        let status = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        failure = match status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(format!(
                "The {} update command exited with {}.",
                args.channel.as_str(),
                status
                    .code()
                    .map_or_else(|| "no status code".to_owned(), |code| code.to_string())
            )),
            Err(error) => Some(format!(
                "Could not start the {} update command: {error}",
                args.channel.as_str()
            )),
        };
        if failure.is_some() {
            break;
        }
    }
    cleanup_direct_installer(
        args.installer.as_deref(),
        &args.result_file,
        &args.target_version,
    );
    if let Some(message) = failure.as_deref() {
        write_failure(&args.result_file, &args.target_version, message)?;
        if let Err(error) = reopen_after_failed_update(&args) {
            write_failure(&args.result_file, &args.target_version, &error)?;
            return Err(error);
        }
    } else {
        if let Err(error) = reopen_after_successful_update(&args) {
            write_failure(&args.result_file, &args.target_version, &error)?;
            return Err(error);
        }
        write_installed(&args.result_file, &args.target_version)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper_fixture(root: &Path, version: &str, channel: Option<&str>) -> PathBuf {
        let executable = root.join("bin/rufin.exe");
        fs::create_dir_all(executable.parent().expect("binary directory")).expect("bin directory");
        fs::write(&executable, b"rufin").expect("Rufin fixture");
        fs::write(root.join("Uninstall.exe"), b"uninstaller").expect("uninstaller fixture");
        if let Some(channel) = channel {
            fs::write(root.join(CHANNEL_FILE), channel).expect("channel fixture");
        }
        let helper_dir = root.join("updater").join(version);
        fs::create_dir_all(&helper_dir).expect("helper directory");
        fs::write(helper_dir.join(HELPER_FILE), b"helper").expect("helper fixture");
        fs::write(helper_dir.join(HELPER_SENTINEL), sentinel_contents(version))
            .expect("sentinel fixture");
        executable
    }

    #[test]
    fn installed_layout_requires_an_explicit_known_channel() {
        let fixture = tempfile::tempdir().expect("fixture");
        let executable = helper_fixture(fixture.path(), "1.2.3", None);
        assert_eq!(
            detect_support_at(&executable, "1.2.3", fixture.path()).expect_err("missing channel"),
            UpdateUnavailable::MissingChannel
        );
        fs::write(fixture.path().join(CHANNEL_FILE), "other\n").expect("unknown channel");
        assert_eq!(
            detect_support_at(&executable, "1.2.3", fixture.path()).expect_err("unknown channel"),
            UpdateUnavailable::UnknownChannel
        );
        fs::write(fixture.path().join(CHANNEL_FILE), "scoop\n").expect("known channel");
        let support =
            detect_support_at(&executable, "1.2.3", fixture.path()).expect("supported layout");
        assert_eq!(support.channel, UpdateChannel::Scoop);
        assert_eq!(support.relaunch, executable);
        assert_eq!(
            support.helper_dir,
            fixture.path().join("updater").join("1.2.3")
        );
        assert_eq!(support.updater_root, fixture.path().join("updater"));
        assert_eq!(support.channel.as_str(), "scoop");
        fs::remove_file(fixture.path().join("Uninstall.exe")).expect("remove uninstaller");
        assert_eq!(
            detect_support_at(&executable, "1.2.3", fixture.path())
                .expect_err("missing uninstaller"),
            UpdateUnavailable::NotInstalled
        );
    }

    #[test]
    fn direct_and_scoop_support_automatic_updates_but_winget_does_not() {
        assert!(channel_supports_automatic_updates(UpdateChannel::Direct));
        assert!(channel_supports_automatic_updates(UpdateChannel::Scoop));
        assert!(!channel_supports_automatic_updates(UpdateChannel::Winget));
    }

    #[test]
    fn copied_complete_layout_is_not_an_installed_updater() {
        let fixture = tempfile::tempdir().expect("fixture");
        let installed_root = fixture.path().join("installed");
        let copied_root = fixture.path().join("copied");
        let installed = helper_fixture(&installed_root, "1.2.3", Some("direct"));
        let copied = helper_fixture(&copied_root, "1.2.3", Some("direct"));

        detect_support_at(&installed, "1.2.3", &installed_root)
            .expect("canonical installed layout");
        assert_eq!(
            detect_support_at(&copied, "1.2.3", &installed_root).expect_err("copied layout"),
            UpdateUnavailable::NotInstalled
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_layout_rejects_a_symlinked_channel_marker() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().expect("fixture");
        let executable = helper_fixture(fixture.path(), "1.2.3", None);
        fs::write(fixture.path().join("channel-target"), "direct\n").expect("channel target");
        symlink("channel-target", fixture.path().join(CHANNEL_FILE)).expect("channel symlink");

        assert_eq!(
            detect_support_at(&executable, "1.2.3", fixture.path()).expect_err("symlinked channel"),
            UpdateUnavailable::UnknownChannel
        );
    }

    #[test]
    fn helper_sentinel_must_match_the_installed_version() {
        let fixture = tempfile::tempdir().expect("fixture");
        let executable = helper_fixture(fixture.path(), "1.2.3", Some("direct"));
        fs::write(
            fixture.path().join("updater/1.2.3").join(HELPER_SENTINEL),
            sentinel_contents("1.2.2"),
        )
        .expect("wrong sentinel");
        assert_eq!(
            detect_support_at(&executable, "1.2.3", fixture.path())
                .expect_err("wrong helper version"),
            UpdateUnavailable::MissingHelper
        );
    }

    #[test]
    fn release_digest_is_optional_and_only_uses_the_exact_asset() {
        let digest = "a".repeat(64);
        let release: GitHubRelease = serde_json::from_value(serde_json::json!({
            "assets": [
                {
                    "name": "Rufin-1.2.3-setup.exe",
                    "browser_download_url": "https://github.com/screwys/Rufin/releases/download/v1.2.3/Rufin-1.2.3-setup.exe",
                    "digest": format!("sha256:{digest}")
                }
            ]
        }))
        .expect("release fixture");
        assert_eq!(optional_release_digest(&release, "1.2.3"), Some(digest));

        let malformed: GitHubRelease = serde_json::from_value(serde_json::json!({
            "assets": [
                {
                    "name": "Rufin-1.2.3-setup.exe",
                    "browser_download_url": "https://github.com/screwys/Rufin/releases/download/v1.2.3/Rufin-1.2.3-setup.exe",
                    "digest": "sha256:not-a-digest"
                }
            ]
        }))
        .expect("release fixture");
        assert_eq!(optional_release_digest(&malformed, "1.2.3"), None);

        let mut hasher = Sha256::new();
        hasher.update(b"Rufin installer");
        assert_eq!(
            lowercase_hex(&hasher.finalize()),
            "b30917d3d80ea09c2634bb7e695e9cb2de9ae2ce5dcb87fb997c89fe892b850b"
        );
    }

    #[test]
    fn each_channel_has_a_fixed_update_plan() {
        let base = HelperArgs {
            parent_pid: 42,
            channel: UpdateChannel::Direct,
            target_version: "1.2.3".to_owned(),
            result_file: PathBuf::from("result.json"),
            relaunch: PathBuf::from("rufin.exe"),
            installer: Some(PathBuf::from("Rufin-1.2.3-setup.exe")),
        };
        assert_eq!(
            command_plan(&base).expect("direct command"),
            vec![CommandSpec {
                program: PathBuf::from("Rufin-1.2.3-setup.exe"),
                args: vec![OsString::from("/S"), OsString::from("/RUFINCHANNEL=direct")],
            }]
        );
        let scoop = HelperArgs {
            channel: UpdateChannel::Scoop,
            installer: None,
            ..base
        };
        assert_eq!(
            command_plan(&scoop).expect("Scoop command"),
            vec![
                CommandSpec {
                    program: PathBuf::from("scoop.cmd"),
                    args: vec![OsString::from("update")],
                },
                CommandSpec {
                    program: PathBuf::from("scoop.cmd"),
                    args: vec![OsString::from("update"), OsString::from("rufin")],
                },
            ]
        );
        let winget = HelperArgs {
            channel: UpdateChannel::Winget,
            ..scoop
        };
        let winget = command_plan(&winget).expect("WinGet command");
        assert_eq!(winget[0].program, PathBuf::from("winget.exe"));
        assert_eq!(
            winget[0].args,
            [
                "upgrade",
                "--id",
                "screwys.Rufin",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--disable-interactivity",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn helper_cleanup_removes_only_complete_old_owned_versions() {
        let fixture = tempfile::tempdir().expect("fixture");
        let updater = fixture.path().join("updater");
        for version in ["1.2.3", "1.2.2", "1.2.1", "1.2.0"] {
            let directory = updater.join(version);
            fs::create_dir_all(&directory).expect("helper version");
            fs::write(directory.join(HELPER_FILE), b"helper").expect("helper executable");
            fs::write(directory.join("runtime.dll"), b"runtime").expect("helper runtime");
            fs::write(directory.join(HELPER_SENTINEL), sentinel_contents(version))
                .expect("helper sentinel");
        }
        fs::write(updater.join("1.2.1/keep.txt"), b"foreign").expect("foreign file");
        fs::create_dir(updater.join("1.2.0/nested")).expect("nested directory");

        cleanup_old_helpers(&updater, "1.2.3");

        assert!(updater.join("1.2.3").exists());
        assert!(!updater.join("1.2.2").exists());
        assert!(updater.join("1.2.1/keep.txt").exists());
        assert!(updater.join("1.2.0/nested").exists());
    }

    #[test]
    fn helper_cleanup_removes_libraries_before_the_executable() {
        let fixture = tempfile::tempdir().expect("fixture");
        let old = fixture.path().join("1.2.2");
        fs::create_dir_all(&old).expect("old helper");
        fs::write(old.join(HELPER_FILE), b"helper").expect("helper executable");
        fs::write(old.join("runtime.dll"), b"runtime").expect("helper runtime");
        fs::write(old.join(HELPER_SENTINEL), sentinel_contents("1.2.2")).expect("helper sentinel");

        let files = owned_old_helper_files(&old, "1.2.2").expect("owned helper files");
        assert_eq!(
            files
                .iter()
                .filter_map(|path| path.file_name())
                .collect::<Vec<_>>(),
            [
                OsStr::new("runtime.dll"),
                OsStr::new(HELPER_FILE),
                OsStr::new(HELPER_SENTINEL),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn helper_cleanup_rejects_symlinked_owned_files() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().expect("fixture");
        let updater = fixture.path().join("updater");
        let old = updater.join("1.2.2");
        fs::create_dir_all(&old).expect("old helper");
        fs::write(old.join(HELPER_FILE), b"helper").expect("helper executable");
        fs::write(old.join(HELPER_SENTINEL), sentinel_contents("1.2.2")).expect("helper sentinel");
        fs::write(fixture.path().join("outside.dll"), b"outside").expect("outside file");
        symlink(fixture.path().join("outside.dll"), old.join("runtime.dll"))
            .expect("runtime symlink");

        cleanup_old_helpers(&updater, "1.2.3");

        assert!(old.exists());
        assert!(fixture.path().join("outside.dll").exists());
    }

    #[test]
    fn helper_arguments_preserve_paths_and_require_the_direct_installer() {
        let parsed = parse_helper_args([
            OsString::from("rufin-update-helper.exe"),
            OsString::from("--parent-pid"),
            OsString::from("42"),
            OsString::from("--channel"),
            OsString::from("direct"),
            OsString::from("--target-version"),
            OsString::from("1.2.3"),
            OsString::from("--result-file"),
            OsString::from("C:\\Rufin cache\\result.json"),
            OsString::from("--relaunch"),
            OsString::from("C:\\Rufin\\bin\\rufin.exe"),
            OsString::from("--installer"),
            OsString::from("C:\\Rufin cache\\Rufin-1.2.3-setup.exe"),
        ])
        .expect("helper arguments");
        assert_eq!(parsed.parent_pid, 42);
        assert_eq!(parsed.target_version, "1.2.3");
        assert_eq!(
            parsed.result_file,
            PathBuf::from("C:\\Rufin cache\\result.json")
        );
        assert_eq!(parsed.relaunch, PathBuf::from("C:\\Rufin\\bin\\rufin.exe"));

        let missing_installer = HelperArgs {
            installer: None,
            ..parsed
        };
        assert!(command_plan(&missing_installer).is_err());
    }

    #[test]
    fn helper_arguments_reject_duplicates_relative_paths_and_channel_extras() {
        let direct = [
            "rufin-update-helper.exe",
            "--parent-pid",
            "42",
            "--channel",
            "direct",
            "--target-version",
            "1.2.3",
            "--result-file",
            "C:\\cache\\result.json",
            "--relaunch",
            "C:\\Rufin\\bin\\rufin.exe",
            "--installer",
            "C:\\cache\\Rufin-1.2.3-setup.exe",
        ];
        let mut duplicate = direct.map(OsString::from).to_vec();
        duplicate.extend([OsString::from("--channel"), OsString::from("scoop")]);
        assert!(parse_helper_args(duplicate).is_err());

        let mut relative = direct.map(OsString::from);
        relative[8] = OsString::from("result.json");
        assert!(parse_helper_args(relative).is_err());

        let mut relative = direct.map(OsString::from);
        relative[10] = OsString::from("rufin.exe");
        assert!(parse_helper_args(relative).is_err());

        let mut relative = direct.map(OsString::from);
        relative[12] = OsString::from("Rufin-1.2.3-setup.exe");
        assert!(parse_helper_args(relative).is_err());

        let mut zero_pid = direct.map(OsString::from);
        zero_pid[2] = OsString::from("0");
        assert!(parse_helper_args(zero_pid).is_err());

        let mut scoop_with_installer = direct.map(OsString::from);
        scoop_with_installer[4] = OsString::from("scoop");
        assert!(parse_helper_args(scoop_with_installer).is_err());

        let mut extra = direct.map(OsString::from).to_vec();
        extra.push(OsString::from("unexpected"));
        assert!(parse_helper_args(extra).is_err());

        let mut unknown = direct.map(OsString::from).to_vec();
        unknown.extend([OsString::from("--other"), OsString::from("value")]);
        assert!(parse_helper_args(unknown).is_err());
    }

    #[test]
    fn previous_installed_result_is_typed_and_consumed_once() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = result_path(fixture.path());
        write_installed(&path, "1.2.3").expect("installed result");

        assert_eq!(
            take_previous_result(fixture.path()).expect("valid result"),
            Some(PreviousUpdateResult::Installed {
                version: "1.2.3".to_owned()
            })
        );
        assert_eq!(
            take_previous_result(fixture.path()).expect("consumed result"),
            None
        );
    }

    #[test]
    fn previous_failure_result_keeps_a_bounded_message() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = result_path(fixture.path());
        write_failure(&path, "1.2.3", &"x".repeat(MAX_FAILURE_MESSAGE_BYTES + 20))
            .expect("failure result");
        let result = take_previous_result(fixture.path())
            .expect("valid result")
            .expect("stored failure");
        let PreviousUpdateResult::Failed { version, message } = result else {
            panic!("expected failed update result");
        };
        assert_eq!(version, "1.2.3");
        assert_eq!(message.len(), MAX_FAILURE_MESSAGE_BYTES);
    }

    #[test]
    fn malformed_and_oversized_results_are_rejected_and_removed() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = result_path(fixture.path());
        fs::create_dir_all(path.parent().expect("result directory")).expect("result directory");

        fs::write(&path, br#"{"status":"installed","version":"../1.2.3"}"#)
            .expect("malformed result");
        assert!(matches!(
            take_previous_result(fixture.path()),
            Err(UpdateError::InvalidResult)
        ));
        assert!(!path.exists());

        let unbounded_failure = serde_json::json!({
            "status": "failed",
            "version": "1.2.3",
            "message": "x".repeat(MAX_FAILURE_MESSAGE_BYTES + 1),
        });
        fs::write(
            &path,
            serde_json::to_vec(&unbounded_failure).expect("failure JSON"),
        )
        .expect("unbounded failure result");
        assert!(matches!(
            take_previous_result(fixture.path()),
            Err(UpdateError::InvalidResult)
        ));
        assert!(!path.exists());

        fs::write(&path, vec![b'x'; MAX_RESULT_BYTES as usize + 1]).expect("oversized result");
        assert!(matches!(
            take_previous_result(fixture.path()),
            Err(UpdateError::InvalidResult)
        ));
        assert!(!path.exists());
    }

    #[test]
    fn direct_installer_cleanup_is_exact_and_preserves_the_result() {
        let fixture = tempfile::tempdir().expect("fixture");
        let update_root = fixture.path().join("windows-update");
        let result = update_root.join(RESULT_FILE);
        let version_dir = update_root.join("downloads/1.2.3");
        let installer = version_dir.join(installer_asset_name("1.2.3"));
        fs::create_dir_all(&version_dir).expect("download directory");
        fs::write(&installer, b"installer").expect("installer");
        fs::write(&result, b"pending result").expect("result");

        cleanup_direct_installer(Some(&installer), &result, "1.2.3");

        assert!(!installer.exists());
        assert!(!version_dir.exists());
        assert_eq!(
            fs::read(&result).expect("preserved result"),
            b"pending result"
        );

        let unrelated = fixture.path().join(installer_asset_name("1.2.3"));
        fs::write(&unrelated, b"unrelated").expect("unrelated installer");
        cleanup_direct_installer(Some(&unrelated), &result, "1.2.3");
        assert!(unrelated.exists());
    }

    #[test]
    fn installer_body_must_be_nonempty_and_bounded() {
        assert!(matches!(
            validate_installer_size(0),
            Err(UpdateError::EmptyInstaller)
        ));
        assert!(validate_installer_size(MAX_INSTALLER_BYTES).is_ok());
        assert!(matches!(
            validate_installer_size(MAX_INSTALLER_BYTES + 1),
            Err(UpdateError::InstallerTooLarge)
        ));
    }

    #[test]
    fn versions_accept_release_tags_but_reject_path_components() {
        assert_eq!(normalize_version("v1.2.3").expect("release tag"), "1.2.3");
        assert!(normalize_version("../1.2.3").is_err());
        assert!(normalize_version("1/2/3").is_err());
    }

    #[test]
    fn relaunch_protocol_accepts_only_owned_status_messages() {
        assert_eq!(RelaunchStatus::Ready.line(), "READY");
        assert_eq!(RelaunchStatus::Present.line(), "PRESENT");
        assert_eq!(RelaunchStatus::Visible.line(), "VISIBLE");
        assert_eq!(
            RelaunchStatus::parse("READY\r\n"),
            Some(RelaunchStatus::Ready)
        );
        assert_eq!(
            RelaunchStatus::parse("PRESENT\n"),
            Some(RelaunchStatus::Present)
        );
        assert_eq!(
            RelaunchStatus::parse("VISIBLE"),
            Some(RelaunchStatus::Visible)
        );
        assert_eq!(RelaunchStatus::parse("ready"), None);
        assert_eq!(RelaunchStatus::parse("VISIBLE extra"), None);
    }
}
