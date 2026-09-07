use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum InstallOutcome {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Updated { restart_required: bool },
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Restarting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum PreviousUpdateResult {
    Installed { version: String },
    Failed { version: String, message: String },
}

impl PreviousUpdateResult {
    pub(super) fn version(&self) -> &str {
        match self {
            Self::Installed { version } | Self::Failed { version, .. } => version,
        }
    }
}

#[derive(Clone)]
pub(super) struct ReleaseInstaller(platform::Installer);

impl ReleaseInstaller {
    pub(super) fn detect(cache_dir: PathBuf) -> Option<Self> {
        platform::detect(cache_dir).map(Self)
    }

    pub(super) fn install(&self, version: &str) -> Result<InstallOutcome, String> {
        platform::install(&self.0, version)
    }

    pub(super) fn supports_automatic_updates(&self) -> bool {
        platform::supports_automatic_updates(&self.0)
    }
}

pub(super) fn take_previous_update_result() -> Option<PreviousUpdateResult> {
    platform::take_previous_update_result()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::HashSet;
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use super::{InstallOutcome, PreviousUpdateResult};

    #[derive(Clone)]
    pub(super) struct Installer {
        brew: PathBuf,
    }

    pub(super) fn detect(_cache_dir: PathBuf) -> Option<Installer> {
        brew_candidates().into_iter().find_map(|brew| {
            let prefix = brew.parent()?.parent()?;
            (brew.is_file() && prefix.join("Caskroom/rufin").is_dir()).then_some(Installer { brew })
        })
    }

    pub(super) fn install(installer: &Installer, version: &str) -> Result<InstallOutcome, String> {
        let update = brew_command(&installer.brew, ["update"]);
        let upgrade = brew_command(&installer.brew, ["upgrade", "--cask", "screwys/tap/rufin"]);
        let installed = brew_command(&installer.brew, ["list", "--cask", "--versions", "rufin"])?;
        if installed.status.success() && super::brew_output_has_version(&installed.stdout, version)
        {
            return Ok(InstallOutcome::Updated {
                restart_required: true,
            });
        }
        if update.is_err() || update.is_ok_and(|output| !output.status.success()) {
            return Err("Homebrew could not refresh its package information.".to_string());
        }
        if upgrade.is_err() || upgrade.is_ok_and(|output| !output.status.success()) {
            return Err("Homebrew could not upgrade Rufin.".to_string());
        }
        Err("Homebrew did not install the requested Rufin release.".to_string())
    }

    pub(super) fn supports_automatic_updates(_installer: &Installer) -> bool {
        true
    }

    pub(super) fn take_previous_update_result() -> Option<PreviousUpdateResult> {
        None
    }

    fn brew_command<const N: usize>(brew: &Path, arguments: [&str; N]) -> Result<Output, String> {
        Command::new(brew)
            .args(arguments)
            .env("HOMEBREW_NO_ANALYTICS", "1")
            .output()
            .map_err(|error| format!("Homebrew could not start: {error}"))
    }

    fn brew_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(prefix) = env::var_os("HOMEBREW_PREFIX") {
            candidates.push(PathBuf::from(prefix).join("bin/brew"));
        }
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/brew"),
            PathBuf::from("/usr/local/bin/brew"),
        ]);
        if let Some(path) = env::var_os("PATH") {
            candidates.extend(
                env::split_paths(&path)
                    .map(|directory| directory.join("brew"))
                    .filter(|candidate| candidate.is_file()),
            );
        }
        let mut seen = HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.clone()));
        candidates
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::path::PathBuf;

    use super::{InstallOutcome, PreviousUpdateResult};

    #[derive(Clone)]
    pub(super) struct Installer {
        inner: windows_updater::InstalledUpdater,
    }

    pub(super) fn detect(cache_dir: PathBuf) -> Option<Installer> {
        windows_updater::InstalledUpdater::detect(cache_dir)
            .ok()
            .flatten()
            .map(|inner| Installer { inner })
    }

    pub(super) fn install(installer: &Installer, version: &str) -> Result<InstallOutcome, String> {
        installer.inner.install(version)?;
        Ok(InstallOutcome::Restarting)
    }

    pub(super) fn supports_automatic_updates(installer: &Installer) -> bool {
        installer.inner.supports_automatic_updates()
    }

    pub(super) fn take_previous_update_result() -> Option<PreviousUpdateResult> {
        windows_updater::take_previous_update_result()
            .ok()
            .flatten()
            .map(|result| match result {
                windows_updater::PreviousUpdateResult::Installed { version } => {
                    PreviousUpdateResult::Installed { version }
                }
                windows_updater::PreviousUpdateResult::Failed { version, message } => {
                    PreviousUpdateResult::Failed { version, message }
                }
            })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use std::path::PathBuf;

    use super::{InstallOutcome, PreviousUpdateResult};

    #[derive(Clone)]
    pub(super) struct Installer;

    pub(super) fn detect(_cache_dir: PathBuf) -> Option<Installer> {
        None
    }

    pub(super) fn install(
        _installer: &Installer,
        _version: &str,
    ) -> Result<InstallOutcome, String> {
        Err("This Rufin package does not provide an in-app updater.".to_string())
    }

    pub(super) fn supports_automatic_updates(_installer: &Installer) -> bool {
        false
    }

    pub(super) fn take_previous_update_result() -> Option<PreviousUpdateResult> {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn brew_output_has_version(output: &[u8], version: &str) -> bool {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .skip(1)
        .any(|installed| installed == version)
}

#[cfg(test)]
mod tests {
    use super::brew_output_has_version;

    #[test]
    fn homebrew_version_check_matches_a_complete_version_token() {
        assert!(brew_output_has_version(b"rufin 0.11.2\n", "0.11.2"));
        assert!(!brew_output_has_version(b"rufin 0.11.20\n", "0.11.2"));
        assert!(!brew_output_has_version(b"rufin\n", "0.11.2"));
    }
}
