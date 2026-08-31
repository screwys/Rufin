#[cfg(any(target_os = "windows", test))]
use std::ffi::OsStr;
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
use serde::Deserialize;
#[cfg(any(target_os = "windows", test))]
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(any(target_os = "windows", test))]
const HELPER_FILE: &str = "rufin-update-helper.exe";
#[cfg(any(target_os = "windows", test))]
const HELPER_SENTINEL: &str = "rufin-update-helper.complete";
const UPDATE_CACHE_DIR: &str = "windows-update";
const RESULT_FILE: &str = "result.json";
#[cfg(target_os = "windows")]
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESULT_BYTES: u64 = 16 * 1024;
#[cfg(any(target_os = "windows", test))]
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FAILURE_MESSAGE_BYTES: usize = 1024;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Completes the restart handshake expected by the released updater helper.
/// The legacy helper receives its final acknowledgement before normal startup
/// so it cannot terminate a slow first launch while users migrate to NSIS-owned
/// updates.
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
    io::stdout()
        .write_all(format!("{}\n", RelaunchStatus::Visible.line()).as_bytes())
        .and_then(|()| io::stdout().flush())
        .map_err(|error| format!("could not complete the legacy update restart: {error}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
enum UpdateUnavailable {
    #[cfg(not(target_os = "windows"))]
    #[error("updates are only available in the installed Windows app")]
    NotWindows,
    #[cfg(any(target_os = "windows", test))]
    #[error("this copy of Rufin is not installed in the Windows package layout")]
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    cache_dir: PathBuf,
}

impl InstalledUpdater {
    /// Detects the direct Windows installation layout. Development and copied
    /// builds deliberately do not gain an updater by inference.
    pub fn detect(cache_dir: PathBuf) -> Result<Option<Self>, String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = cache_dir;
            Ok(None)
        }

        #[cfg(target_os = "windows")]
        {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let install_root = directories::BaseDirs::new()
                .map(|dirs| dirs.data_local_dir().join("Programs").join(PROJECT_NAME))
                .and_then(|expected| detect_install_root(&executable, &expected).ok());
            if let Some(install_root) = install_root.as_ref() {
                cleanup_legacy_helpers(&install_root.join("updater"));
                cleanup_downloaded_installers(&update_root(&cache_dir));
            }
            Ok(install_root.map(|_| Self { cache_dir }))
        }
    }

    /// Starts the downloaded installer in automatic-update mode. NSIS waits
    /// for Rufin to exit, replaces the installation, and relaunches it.
    pub fn install(&self, version: &str) -> Result<(), String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = &self.cache_dir;
            let _ = version;
            Err(UpdateUnavailable::NotWindows.to_string())
        }

        #[cfg(target_os = "windows")]
        {
            start_installed_update(version, &self.cache_dir).map_err(|error| error.to_string())
        }
    }

    /// The direct installer completes updates without user interaction.
    #[must_use]
    pub fn supports_automatic_updates(&self) -> bool {
        #[cfg(not(target_os = "windows"))]
        {
            false
        }

        #[cfg(target_os = "windows")]
        {
            true
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
    #[error("the previous update result is invalid")]
    InvalidResult,
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

#[cfg(target_os = "windows")]
fn start_installed_update(target_version: &str, cache_dir: &Path) -> Result<(), UpdateError> {
    let target_version = normalize_version(target_version)?;
    let update_root = update_root(cache_dir);
    fs::create_dir_all(&update_root)?;
    remove_file_if_present(&result_path(cache_dir))?;

    let installer = download_installer(&target_version, &update_root)?;
    let mut command = Command::new(&installer);
    command
        .current_dir(installer.parent().unwrap_or(&update_root))
        .arg("/S")
        .arg("/RUFINUPDATE=1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command.spawn()?;
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

/// Removes only complete helper directories left by released installers.
/// Failures are ignored because the helper that launched this version may
/// still have its executable open; the next launch retries cleanup.
#[cfg(any(target_os = "windows", test))]
fn cleanup_legacy_helpers(updater_root: &Path) {
    let Ok(entries) = fs::read_dir(updater_root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(files) = owned_old_helper_files(&entry.path(), &version) {
            for file in files {
                if fs::remove_file(file).is_err() {
                    break;
                }
            }
            let _ = fs::remove_dir(entry.path());
        }
    }
    let _ = fs::remove_dir(updater_root);
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
fn detect_install_root(
    executable: &Path,
    expected_install_root: &Path,
) -> Result<PathBuf, UpdateUnavailable> {
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
    Ok(install_root.to_path_buf())
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
fn cleanup_downloaded_installers(update_root: &Path) {
    let downloads = update_root.join("downloads");
    let Ok(entries) = fs::read_dir(&downloads) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let version_dir = entry.path();
        if normalize_version(&version).ok().as_deref() != Some(version.as_str())
            || !regular_owned_directory(&version_dir)
        {
            continue;
        }
        let installer = version_dir.join(installer_asset_name(&version));
        if regular_owned_file(&installer) {
            let _ = fs::remove_file(installer);
        }
        let _ = fs::remove_dir(version_dir);
    }
    let _ = fs::remove_dir(downloads);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed_fixture(root: &Path) -> PathBuf {
        let executable = root.join("bin/rufin.exe");
        fs::create_dir_all(executable.parent().expect("binary directory")).expect("bin directory");
        fs::write(&executable, b"rufin").expect("Rufin fixture");
        fs::write(root.join("Uninstall.exe"), b"uninstaller").expect("uninstaller fixture");
        executable
    }

    fn legacy_helper_fixture(root: &Path, version: &str) {
        let helper_dir = root.join("updater").join(version);
        fs::create_dir_all(&helper_dir).expect("helper directory");
        fs::write(helper_dir.join(HELPER_FILE), b"helper").expect("helper fixture");
        fs::write(helper_dir.join(HELPER_SENTINEL), sentinel_contents(version))
            .expect("sentinel fixture");
    }

    #[test]
    fn installed_layout_requires_the_canonical_root_and_uninstaller() {
        let fixture = tempfile::tempdir().expect("fixture");
        let executable = installed_fixture(fixture.path());
        assert_eq!(
            detect_install_root(&executable, fixture.path()).expect("installed layout"),
            fixture.path()
        );
        fs::remove_file(fixture.path().join("Uninstall.exe")).expect("remove uninstaller");
        assert_eq!(
            detect_install_root(&executable, fixture.path()).expect_err("missing uninstaller"),
            UpdateUnavailable::NotInstalled
        );
    }

    #[test]
    fn copied_complete_layout_is_not_an_installed_updater() {
        let fixture = tempfile::tempdir().expect("fixture");
        let installed_root = fixture.path().join("installed");
        let copied_root = fixture.path().join("copied");
        let installed = installed_fixture(&installed_root);
        let copied = installed_fixture(&copied_root);

        detect_install_root(&installed, &installed_root).expect("canonical installed layout");
        assert_eq!(
            detect_install_root(&copied, &installed_root).expect_err("copied layout"),
            UpdateUnavailable::NotInstalled
        );
    }

    #[test]
    fn legacy_cleanup_preserves_a_helper_with_the_wrong_sentinel() {
        let fixture = tempfile::tempdir().expect("fixture");
        legacy_helper_fixture(fixture.path(), "1.2.3");
        fs::write(
            fixture.path().join("updater/1.2.3").join(HELPER_SENTINEL),
            sentinel_contents("1.2.2"),
        )
        .expect("wrong sentinel");
        cleanup_legacy_helpers(&fixture.path().join("updater"));
        assert!(fixture.path().join("updater/1.2.3").exists());
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
    fn legacy_cleanup_removes_only_complete_owned_versions() {
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

        cleanup_legacy_helpers(&updater);

        assert!(!updater.join("1.2.3").exists());
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

        cleanup_legacy_helpers(&updater);

        assert!(old.exists());
        assert!(fixture.path().join("outside.dll").exists());
    }

    #[test]
    fn previous_installed_result_is_typed_and_consumed_once() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = result_path(fixture.path());
        fs::create_dir_all(path.parent().expect("result directory")).expect("result directory");
        fs::write(&path, br#"{"status":"installed","version":"1.2.3"}"#).expect("installed result");

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
    fn previous_failure_result_accepts_the_legacy_message_bound() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = result_path(fixture.path());
        fs::create_dir_all(path.parent().expect("result directory")).expect("result directory");
        let failure = serde_json::json!({
            "status": "failed",
            "version": "1.2.3",
            "message": "x".repeat(MAX_FAILURE_MESSAGE_BYTES),
        });
        fs::write(&path, serde_json::to_vec(&failure).expect("failure JSON"))
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
    fn downloaded_installer_cleanup_is_exact() {
        let fixture = tempfile::tempdir().expect("fixture");
        let update_root = fixture.path().join("windows-update");
        let version_dir = update_root.join("downloads/1.2.3");
        let installer = version_dir.join(installer_asset_name("1.2.3"));
        fs::create_dir_all(&version_dir).expect("download directory");
        fs::write(&installer, b"installer").expect("installer");
        let unrelated_dir = update_root.join("downloads/other");
        fs::create_dir_all(&unrelated_dir).expect("unrelated directory");
        fs::write(unrelated_dir.join("keep.txt"), b"unrelated").expect("unrelated file");

        cleanup_downloaded_installers(&update_root);

        assert!(!installer.exists());
        assert!(!version_dir.exists());
        assert!(unrelated_dir.join("keep.txt").exists());
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
