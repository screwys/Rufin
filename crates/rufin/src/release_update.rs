//! Cached release history and platform-owned updates.
//!
//! GitHub Releases is the single release-history source. Rufin caches the complete
//! presentation, refreshes it at most every six hours, and keeps one-time
//! notification receipts separate from persistent update availability.

mod install;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_channel::Sender;
use serde::{Deserialize, Serialize};
use tracing::debug;
use ui::runtime::{ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdatePort};

use self::install::{InstallOutcome, ReleaseInstaller};
use crate::settings::SettingsFile;

const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/screwys/Rufin/releases?per_page=5";
const RELEASE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
struct FetchedReleaseUpdate {
    notes: Vec<ReleaseNote>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ReleaseCache {
    notes: Vec<ReleaseNote>,
}

pub(crate) struct ReleaseUpdateOwner {
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    events: Sender<ReleaseUpdate>,
    cache_path: PathBuf,
    cache: Arc<Mutex<ReleaseCache>>,
    effective_installed_version: Arc<Mutex<String>>,
    installer: Option<ReleaseInstaller>,
    automatic_update_blocked_version: Option<String>,
    automatic_update_requested: Arc<AtomicBool>,
    refresh_in_flight: Arc<AtomicBool>,
    update_in_flight: Arc<AtomicBool>,
    previous_update_result: Mutex<Option<install::PreviousUpdateResult>>,
}

impl ReleaseUpdateOwner {
    pub(crate) fn new(
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        events: Sender<ReleaseUpdate>,
        cache_path: PathBuf,
        take_previous_update_result: bool,
    ) -> Arc<Self> {
        let cache = match read_release_cache(&cache_path) {
            Ok(cache) => cache.unwrap_or_default(),
            Err(error) => {
                debug!(%error, path = %cache_path.display(), "could not read cached release history");
                ReleaseCache::default()
            }
        };
        let cache_dir = cache_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let installer = ReleaseInstaller::detect(cache_dir);
        let installed_version = env!("CARGO_PKG_VERSION").to_string();
        let previous_update_result = take_previous_update_result
            .then(install::take_previous_update_result)
            .flatten();
        let automatic_update_blocked_version = previous_update_result
            .as_ref()
            .filter(|result| previous_update_blocks_automatic(result, &installed_version))
            .map(|result| result.version().to_string());
        Arc::new(Self {
            settings,
            runtime,
            events,
            cache_path,
            cache: Arc::new(Mutex::new(cache)),
            effective_installed_version: Arc::new(Mutex::new(installed_version)),
            installer,
            automatic_update_blocked_version,
            automatic_update_requested: Arc::new(AtomicBool::new(false)),
            refresh_in_flight: Arc::new(AtomicBool::new(false)),
            update_in_flight: Arc::new(AtomicBool::new(false)),
            previous_update_result: Mutex::new(previous_update_result),
        })
    }

    pub(crate) fn initial_history(&self) -> ReleaseHistory {
        let installed_version = mutex_lock(&self.effective_installed_version).clone();
        release_history(
            &mutex_lock(&self.cache),
            self.installer.is_some(),
            self.automatic_updates_supported(),
            &installed_version,
        )
    }

    fn automatic_updates_supported(&self) -> bool {
        self.installer
            .as_ref()
            .is_some_and(ReleaseInstaller::supports_automatic_updates)
    }

    fn publish_previous_update_result(&self) {
        let Some(result) = mutex_lock(&self.previous_update_result).take() else {
            return;
        };
        let target_version = result.version().to_string();
        let installed_version = mutex_lock(&self.effective_installed_version).clone();
        if let Some(update) = previous_update_feedback(result, &installed_version) {
            mark_release_notification_seen_best_effort(&self.settings, &target_version);
            let _ = self.events.try_send(update);
        }
    }

    fn check_with_automatic_update(&self, automatically_update: bool) {
        self.publish_previous_update_result();
        if !release_check_allowed(&self.settings.load().ui) {
            return;
        }
        if automatically_update {
            self.automatic_update_requested
                .store(true, Ordering::Release);
        }

        if self.refresh_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }

        let settings = self.settings.clone();
        let events = self.events.clone();
        let cache_path = self.cache_path.clone();
        let cache = Arc::clone(&self.cache);
        let effective_installed_version = Arc::clone(&self.effective_installed_version);
        let refresh_in_flight = Arc::clone(&self.refresh_in_flight);
        let update_in_flight = Arc::clone(&self.update_in_flight);
        let automatic_update_requested = Arc::clone(&self.automatic_update_requested);
        let installer = self.installer.clone();
        let updates_supported = installer.is_some();
        let automatic_updates_supported = installer
            .as_ref()
            .is_some_and(ReleaseInstaller::supports_automatic_updates);
        let automatic_update_blocked_version = self.automatic_update_blocked_version.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn(async move {
            let fetched = match tokio::task::spawn_blocking(fetch_github_release_update).await {
                Ok(Ok(update)) => update,
                Ok(Err(error)) => {
                    debug!(%error, "failed to check GitHub release history");
                    None
                }
                Err(error) => {
                    debug!(%error, "GitHub release check task failed");
                    None
                }
            };
            let refreshed = {
                let previous = mutex_lock(&cache).clone();
                release_cache_after_check(previous, fetched)
            };
            if let Err(error) = write_release_cache(&cache_path, &refreshed) {
                debug!(%error, path = %cache_path.display(), "could not cache release history");
            }
            *mutex_lock(&cache) = refreshed.clone();

            let current_settings = settings.load();
            let installed_version = mutex_lock(&effective_installed_version).clone();
            let automatically_update = automatic_update_requested.swap(false, Ordering::AcqRel);
            let automatic_update = automatically_update
                .then(|| {
                    reserve_automatic_update(
                        &refreshed,
                        &current_settings.ui,
                        installer.as_ref(),
                        &installed_version,
                        automatic_update_blocked_version.as_deref(),
                        &update_in_flight,
                    )
                })
                .flatten();
            let notification_version = release_notification_version(
                &refreshed,
                &current_settings.ui,
                &installed_version,
                update_in_flight.load(Ordering::Acquire),
            );
            let history = release_history(
                &refreshed,
                updates_supported,
                automatic_updates_supported,
                &installed_version,
            );
            let _ = events
                .send(ReleaseUpdate::Refreshed {
                    history,
                    notification_version,
                })
                .await;
            refresh_in_flight.store(false, Ordering::Release);
            if let Some((installer, version)) = automatic_update {
                run_update(
                    settings,
                    runtime,
                    events,
                    effective_installed_version,
                    update_in_flight,
                    installer,
                    version,
                );
            }
        });
    }
}

impl ReleaseUpdatePort for ReleaseUpdateOwner {
    fn check(&self) {
        self.check_with_automatic_update(false);
    }

    fn check_and_update(&self) {
        self.check_with_automatic_update(true);
    }

    fn update(&self, version: String) {
        let version = version.trim().to_string();
        let installed_version = mutex_lock(&self.effective_installed_version).clone();
        let available = release_update_available(
            &mutex_lock(&self.cache),
            self.installer.is_some(),
            &installed_version,
        ) == Some(version.as_str());
        let Some(installer) = self.installer.clone().filter(|_| available) else {
            let _ = self.events.try_send(ReleaseUpdate::Failed {
                version,
                error: "This release cannot be installed from the current Rufin package."
                    .to_string(),
            });
            return;
        };
        if self.update_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        run_update(
            self.settings.clone(),
            self.runtime.clone(),
            self.events.clone(),
            Arc::clone(&self.effective_installed_version),
            Arc::clone(&self.update_in_flight),
            installer,
            version,
        );
    }

    fn mark_seen(&self, version: String) -> Result<(), String> {
        mark_release_notification_seen(&self.settings, &version)
    }
}

fn run_update(
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    events: Sender<ReleaseUpdate>,
    effective_installed_version: Arc<Mutex<String>>,
    update_in_flight: Arc<AtomicBool>,
    installer: ReleaseInstaller,
    version: String,
) {
    mark_release_notification_seen_best_effort(&settings, &version);
    let _ = events.try_send(ReleaseUpdate::Updating {
        version: version.clone(),
    });
    runtime.spawn(async move {
        let install_version = version.clone();
        let result = tokio::task::spawn_blocking(move || installer.install(&install_version))
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
        let update = match result {
            Ok(InstallOutcome::Updated { restart_required }) => {
                *mutex_lock(&effective_installed_version) = version.clone();
                ReleaseUpdate::Updated {
                    version,
                    restart_required,
                }
            }
            Ok(InstallOutcome::Restarting) => ReleaseUpdate::Restarting { version },
            Err(error) => ReleaseUpdate::Failed { version, error },
        };
        let restarting = matches!(update, ReleaseUpdate::Restarting { .. });
        let _ = events.send(update).await;
        if !restarting {
            update_in_flight.store(false, Ordering::Release);
        }
    });
}

fn mutex_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn previous_update_feedback(
    result: install::PreviousUpdateResult,
    installed_version: &str,
) -> Option<ReleaseUpdate> {
    let target_version = result.version().to_string();
    if let install::PreviousUpdateResult::Failed { version, message } = &result {
        if release_version_is_newer(installed_version, version) {
            return None;
        }
        return Some(ReleaseUpdate::Failed {
            version: version.clone(),
            error: message.clone(),
        });
    }
    if release_versions_equal(&target_version, installed_version) {
        return Some(ReleaseUpdate::Updated {
            version: target_version,
            restart_required: false,
        });
    }
    if release_version_is_newer(installed_version, &target_version) {
        return None;
    }
    let install::PreviousUpdateResult::Installed { version } = result else {
        unreachable!("failed update results return before version reconciliation")
    };
    Some(ReleaseUpdate::Failed {
        error: format!("The update command finished, but Rufin {version} was not installed."),
        version,
    })
}

fn previous_update_blocks_automatic(
    result: &install::PreviousUpdateResult,
    installed_version: &str,
) -> bool {
    !release_versions_equal(result.version(), installed_version)
        && !release_version_is_newer(installed_version, result.version())
}

fn release_cache_after_check(
    previous: ReleaseCache,
    fetched: Option<FetchedReleaseUpdate>,
) -> ReleaseCache {
    match fetched {
        Some(fetched) => ReleaseCache {
            notes: fetched.notes,
        },
        None => previous,
    }
}

fn latest_release(cache: &ReleaseCache) -> Option<&str> {
    cache
        .notes
        .first()
        .map(|note| note.version.trim())
        .filter(|version| !version.is_empty())
}

fn release_update_available<'a>(
    cache: &'a ReleaseCache,
    updates_supported: bool,
    installed_version: &str,
) -> Option<&'a str> {
    latest_release(cache)
        .filter(|latest| updates_supported && release_version_is_newer(latest, installed_version))
}

fn automatic_update_target<'a>(
    cache: &'a ReleaseCache,
    settings: &ui::Settings,
    automatic_updates_supported: bool,
    installed_version: &str,
    blocked_version: Option<&str>,
) -> Option<&'a str> {
    if !settings.automatic_updates_enabled
        || !release_check_allowed(settings)
        || !automatic_updates_supported
    {
        return None;
    }
    release_update_available(cache, true, installed_version)
        .filter(|version| Some(*version) != blocked_version)
}

fn reserve_automatic_update(
    cache: &ReleaseCache,
    settings: &ui::Settings,
    installer: Option<&ReleaseInstaller>,
    installed_version: &str,
    blocked_version: Option<&str>,
    update_in_flight: &AtomicBool,
) -> Option<(ReleaseInstaller, String)> {
    let installer = installer.filter(|installer| installer.supports_automatic_updates())?;
    let version =
        automatic_update_target(cache, settings, true, installed_version, blocked_version)?;
    if update_in_flight.swap(true, Ordering::AcqRel) {
        return None;
    }
    Some((installer.clone(), version.to_string()))
}

fn release_history(
    cache: &ReleaseCache,
    updates_supported: bool,
    automatic_updates_supported: bool,
    installed_version: &str,
) -> ReleaseHistory {
    ReleaseHistory {
        notes: cache.notes.clone().into(),
        installed_version: installed_version.to_string(),
        available_version: release_update_available(cache, updates_supported, installed_version)
            .map(str::to_string),
        automatic_updates_supported,
    }
}

fn read_release_cache(path: &Path) -> Result<Option<ReleaseCache>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_release_cache(path: &Path, cache: &ReleaseCache) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Release cache has no parent directory.".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.part");
    let bytes = serde_json::to_vec(cache).map_err(|error| error.to_string())?;
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    if path.is_file() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn fetch_github_release_update() -> Result<Option<FetchedReleaseUpdate>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(RELEASE_CHECK_TIMEOUT)
        .user_agent(format!("Rufin/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    debug!(
        service = "github",
        method = "GET",
        public_url = GITHUB_RELEASES_URL,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client
        .get(GITHUB_RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|error| error.to_string())?;
    debug!(
        service = "github",
        method = "GET",
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "received remote response"
    );
    let value = response
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json::<serde_json::Value>()
        .map_err(|error| error.to_string())?;
    Ok(release_update_from_github_json(&value))
}

fn release_update_from_github_json(value: &serde_json::Value) -> Option<FetchedReleaseUpdate> {
    let mut notes: Vec<_> = value
        .as_array()?
        .iter()
        .filter(|release| {
            !release
                .get("draft")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && !release
                    .get("prerelease")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(release_note_from_github_json)
        .collect();
    notes.sort_by(|left, right| {
        release_version_parts(&right.version).cmp(&release_version_parts(&left.version))
    });
    notes.truncate(5);
    notes.first()?;
    Some(FetchedReleaseUpdate { notes })
}

fn release_note_from_github_json(value: &serde_json::Value) -> Option<ReleaseNote> {
    let tag = value.get("tag_name")?.as_str()?.trim();
    let version = tag.strip_prefix('v').unwrap_or(tag).trim();
    if version.is_empty() {
        return None;
    }
    let published_at = value.get("published_at")?.as_str()?.trim();
    let date = published_at.get(..10).unwrap_or(published_at).to_string();
    let url = value.get("html_url")?.as_str()?.trim();
    if url.is_empty() {
        return None;
    }
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    Some(ReleaseNote {
        version: version.to_string(),
        date,
        url: url.to_string(),
        body: body.to_string(),
    })
}

fn release_check_allowed(settings: &ui::Settings) -> bool {
    !settings.private_mode
}

fn release_notification_due(settings: &ui::Settings, latest: &str, current: &str) -> bool {
    settings.release_notifications_enabled
        && release_check_allowed(settings)
        && release_version_is_newer(latest, current)
        && settings.release_notification_seen_version.as_deref() != Some(latest)
}

fn release_notification_version(
    cache: &ReleaseCache,
    settings: &ui::Settings,
    installed_version: &str,
    update_in_flight: bool,
) -> Option<String> {
    if update_in_flight {
        return None;
    }
    latest_release(cache).and_then(|latest| {
        release_notification_due(settings, latest, installed_version).then(|| latest.to_string())
    })
}

fn mark_release_notification_seen(settings: &SettingsFile, version: &str) -> Result<(), String> {
    let version = version.trim();
    if version.is_empty()
        || settings
            .load()
            .ui
            .release_notification_seen_version
            .as_deref()
            == Some(version)
    {
        return Ok(());
    }
    settings.update(|stored| {
        stored.ui.release_notification_seen_version = Some(version.to_string());
        Ok(())
    })
}

fn mark_release_notification_seen_best_effort(settings: &SettingsFile, version: &str) {
    if let Err(error) = mark_release_notification_seen(settings, version) {
        debug!(%error, %version, "could not record the release notification receipt");
    }
}

fn release_version_is_newer(latest: &str, current: &str) -> bool {
    let Some(latest_parts) = release_version_parts(latest) else {
        return false;
    };
    let Some(current_parts) = release_version_parts(current) else {
        return false;
    };
    let len = latest_parts.len().max(current_parts.len());
    for index in 0..len {
        let latest_part = latest_parts.get(index).copied().unwrap_or(0);
        let current_part = current_parts.get(index).copied().unwrap_or(0);
        if latest_part != current_part {
            return latest_part > current_part;
        }
    }
    false
}

fn release_versions_equal(left: &str, right: &str) -> bool {
    release_version_parts(left)
        .zip(release_version_parts(right))
        .is_some_and(|(left, right)| left == right)
}

fn release_version_parts(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().strip_prefix('v').unwrap_or(version.trim());
    let version = version.split(['-', '+']).next().unwrap_or(version);
    if version.is_empty() {
        return None;
    }
    version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::install::PreviousUpdateResult;
    use super::{
        FetchedReleaseUpdate, ReleaseCache, automatic_update_target,
        mark_release_notification_seen, previous_update_blocks_automatic, previous_update_feedback,
        read_release_cache, release_cache_after_check, release_check_allowed, release_history,
        release_notification_due, release_notification_version, release_update_available,
        release_update_from_github_json, release_version_is_newer, write_release_cache,
    };
    use crate::settings::SettingsFile;
    use ui::runtime::{ReleaseNote, ReleaseUpdate};

    fn note(version: &str) -> ReleaseNote {
        ReleaseNote {
            version: version.to_string(),
            date: String::new(),
            url: format!("https://github.com/screwys/Rufin/releases/tag/v{version}"),
            body: String::new(),
        }
    }

    #[test]
    fn failed_checks_retain_cached_history() {
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };

        let checked = release_cache_after_check(cache.clone(), None);

        assert_eq!(checked, cache);
    }

    #[test]
    fn successful_checks_replace_the_cached_snapshot() {
        let previous = ReleaseCache {
            notes: vec![note("1.0.0")],
        };
        let fetched = FetchedReleaseUpdate {
            notes: vec![note("2.0.0")],
        };

        let checked = release_cache_after_check(previous, Some(fetched));

        assert_eq!(checked.notes, vec![note("2.0.0")]);
    }

    #[test]
    fn cached_history_round_trips_without_bundled_release_notes() {
        let directory = tempfile::tempdir().expect("temporary release cache");
        let path = directory.path().join("releases.json");
        let cache = ReleaseCache {
            notes: vec![ReleaseNote {
                version: "2.0.0".to_string(),
                date: "2026-01-02".to_string(),
                url: "https://github.com/screwys/Rufin/releases/tag/v2.0.0".to_string(),
                body: "Summary\n\n- Item".to_string(),
            }],
        };

        write_release_cache(&path, &cache).expect("write release cache");

        assert_eq!(
            read_release_cache(&path).expect("read release cache"),
            Some(cache)
        );
    }

    #[test]
    fn update_availability_requires_a_supported_owner_and_newest_row() {
        let cache = ReleaseCache {
            notes: vec![note("2.0.0"), note("1.0.0")],
        };

        assert_eq!(
            release_history(&cache, true, true, "1.0.0")
                .available_version
                .as_deref(),
            Some("2.0.0")
        );
        assert_eq!(
            release_history(&cache, false, false, "1.0.0").available_version,
            None
        );
        assert_eq!(
            release_history(&cache, true, true, "2.0.0").available_version,
            None
        );
        assert!(release_history(&cache, true, true, "1.0.0").automatic_updates_supported);
        assert!(!release_history(&cache, true, false, "1.0.0").automatic_updates_supported);
        assert_eq!(release_update_available(&cache, true, "2.0.0"), None);
    }

    #[test]
    fn automatic_update_requires_opt_in_owner_support_and_an_unblocked_target() {
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };
        let mut settings = ui::Settings {
            automatic_updates_enabled: true,
            ..ui::Settings::default()
        };

        assert_eq!(
            automatic_update_target(&cache, &settings, true, "1.0.0", None),
            Some("2.0.0")
        );
        assert_eq!(
            automatic_update_target(&cache, &settings, false, "1.0.0", None),
            None
        );
        assert_eq!(
            automatic_update_target(&cache, &settings, true, "1.0.0", Some("2.0.0")),
            None
        );
        assert_eq!(
            automatic_update_target(&cache, &settings, true, "2.0.0", None),
            None
        );

        settings.automatic_updates_enabled = false;
        assert_eq!(
            automatic_update_target(&cache, &settings, true, "1.0.0", None),
            None
        );
        settings.automatic_updates_enabled = true;
        settings.private_mode = true;
        assert_eq!(
            automatic_update_target(&cache, &settings, true, "1.0.0", None),
            None
        );
    }

    #[test]
    fn a_consumed_update_result_reports_the_relaunched_binary_once() {
        let installed = PreviousUpdateResult::Installed {
            version: "2.0.0".to_string(),
        };
        assert_eq!(
            previous_update_feedback(installed.clone(), "2.0.0"),
            Some(ReleaseUpdate::Updated {
                version: "2.0.0".to_string(),
                restart_required: false,
            })
        );
        assert!(!previous_update_blocks_automatic(&installed, "2.0.0"));
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };
        assert_eq!(
            release_notification_version(&cache, &ui::Settings::default(), "2.0.0", false),
            None
        );

        let no_op = previous_update_feedback(installed.clone(), "1.0.0")
            .expect("old binary reports the no-op");
        assert!(matches!(
            no_op,
            ReleaseUpdate::Failed { version, .. } if version == "2.0.0"
        ));
        assert!(previous_update_blocks_automatic(&installed, "1.0.0"));

        let failed_relaunch = PreviousUpdateResult::Failed {
            version: "2.0.0".to_string(),
            message: "Rufin did not present its updated window.".to_string(),
        };
        assert_eq!(
            previous_update_feedback(failed_relaunch, "2.0.0"),
            Some(ReleaseUpdate::Failed {
                version: "2.0.0".to_string(),
                error: "Rufin did not present its updated window.".to_string(),
            })
        );

        let superseded = PreviousUpdateResult::Failed {
            version: "1.5.0".to_string(),
            message: "old failure".to_string(),
        };
        assert_eq!(previous_update_feedback(superseded, "2.0.0"), None);
    }

    #[test]
    fn an_update_in_flight_owns_feedback_instead_of_the_release_toast() {
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };
        let settings = ui::Settings::default();

        assert_eq!(
            release_notification_version(&cache, &settings, "1.0.0", false).as_deref(),
            Some("2.0.0")
        );
        assert_eq!(
            release_notification_version(&cache, &settings, "1.0.0", true),
            None
        );
        assert_eq!(settings.release_notification_seen_version, None);
    }

    #[test]
    fn release_notification_uses_notification_private_and_seen_settings() {
        let mut settings = ui::Settings::default();

        assert!(release_notification_due(&settings, "2.0.0", "1.9.0"));
        assert!(!release_notification_due(&settings, "2.0.0", "2.0.0"));
        settings.release_notification_seen_version = Some("2.0.0".to_string());
        assert!(!release_notification_due(&settings, "2.0.0", "1.9.0"));
        settings.release_notification_seen_version = None;
        settings.release_notifications_enabled = false;
        assert!(!release_notification_due(&settings, "2.0.0", "1.9.0"));
        settings.release_notifications_enabled = true;
        settings.private_mode = true;
        assert!(!release_notification_due(&settings, "2.0.0", "1.9.0"));
    }

    #[test]
    fn release_acquisition_is_gated_only_by_private_mode() {
        let mut settings = ui::Settings {
            release_notifications_enabled: false,
            ..ui::Settings::default()
        };

        assert!(release_check_allowed(&settings));
        settings.private_mode = true;
        assert!(!release_check_allowed(&settings));
    }

    #[test]
    fn notification_preferences_do_not_remove_update_availability() {
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };
        let mut settings = ui::Settings {
            release_notifications_enabled: false,
            ..ui::Settings::default()
        };

        assert!(!release_notification_due(&settings, "2.0.0", "1.0.0"));
        assert_eq!(
            release_history(&cache, true, true, "1.0.0")
                .available_version
                .as_deref(),
            Some("2.0.0")
        );

        settings.release_notifications_enabled = true;
        settings.release_notification_seen_version = Some("2.0.0".to_string());
        assert!(!release_notification_due(&settings, "2.0.0", "1.0.0"));
        assert_eq!(
            release_history(&cache, true, true, "1.0.0")
                .available_version
                .as_deref(),
            Some("2.0.0")
        );
    }

    #[test]
    fn release_versions_compare_numeric_segments() {
        assert!(release_version_is_newer("v2.0.0", "1.9.9"));
        assert!(release_version_is_newer("1.10.0", "1.9.9"));
        assert!(release_version_is_newer("1.0.1", "1.0"));
        assert!(!release_version_is_newer("1.0.0", "1.0"));
        assert!(!release_version_is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn github_json_keeps_complete_stable_release_bodies() {
        let value = serde_json::json!([
            {
                "tag_name": "v1.0.0",
                "published_at": "2026-01-01T00:00:00Z",
                "html_url": "https://github.com/screwys/Rufin/releases/tag/v1.0.0",
                "draft": false,
                "prerelease": false,
                "body": "Older item"
            },
            {
                "tag_name": "v2.0.0",
                "published_at": "2026-06-28T12:34:56Z",
                "html_url": "https://github.com/screwys/Rufin/releases/tag/v2.0.0",
                "draft": false,
                "prerelease": false,
                "body": "Summary & context.\n\n## Changelog\n\n- First item by @someone in #123"
            },
            {
                "tag_name": "v2.1.0-beta.1",
                "published_at": "2026-07-01T12:34:56Z",
                "html_url": "https://github.com/screwys/Rufin/releases/tag/v2.1.0-beta.1",
                "draft": false,
                "prerelease": true,
                "body": "Preview"
            }
        ]);

        assert_eq!(
            release_update_from_github_json(&value),
            Some(FetchedReleaseUpdate {
                notes: vec![
                    ReleaseNote {
                        version: "2.0.0".to_string(),
                        date: "2026-06-28".to_string(),
                        url: "https://github.com/screwys/Rufin/releases/tag/v2.0.0".to_string(),
                        body:
                            "Summary & context.\n\n## Changelog\n\n- First item by @someone in #123"
                                .to_string(),
                    },
                    ReleaseNote {
                        version: "1.0.0".to_string(),
                        date: "2026-01-01".to_string(),
                        url: "https://github.com/screwys/Rufin/releases/tag/v1.0.0".to_string(),
                        body: "Older item".to_string(),
                    }
                ],
            })
        );
    }

    #[test]
    fn github_history_keeps_only_the_five_newest_releases() {
        let releases = (1..=6)
            .map(|version| {
                serde_json::json!({
                    "tag_name": format!("v1.0.{version}"),
                    "published_at": "2026-06-28T12:34:56Z",
                    "html_url": format!(
                        "https://github.com/screwys/Rufin/releases/tag/v1.0.{version}"
                    ),
                    "draft": false,
                    "prerelease": false,
                    "body": format!("Release {version}")
                })
            })
            .collect::<Vec<_>>();

        let history = release_update_from_github_json(&serde_json::Value::Array(releases))
            .expect("release history");

        assert_eq!(history.notes.len(), 5);
        assert_eq!(history.notes[0].version, "1.0.6");
        assert_eq!(history.notes[4].version, "1.0.2");
    }

    #[test]
    fn seen_release_update_preserves_other_settings_and_reopens() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let path = directory.path().join("settings.json");
        let settings = SettingsFile::open(path.clone()).expect("open settings");
        settings
            .update(|stored| {
                stored.ui.private_mode = true;
                stored.ui.notifications_enabled = true;
                Ok(())
            })
            .expect("prepare unrelated settings");

        mark_release_notification_seen(&settings, " 2.0.0 ").expect("mark release seen");

        let reopened = SettingsFile::open(path).expect("reopen settings").load();
        assert_eq!(
            reopened.ui.release_notification_seen_version.as_deref(),
            Some("2.0.0")
        );
        assert!(reopened.ui.private_mode);
        assert!(reopened.ui.notifications_enabled);
    }

    #[test]
    fn update_feedback_owns_the_same_release_notification() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let settings =
            SettingsFile::open(directory.path().join("settings.json")).expect("open settings");
        let cache = ReleaseCache {
            notes: vec![note("2.0.0")],
        };

        mark_release_notification_seen(&settings, "2.0.0").expect("claim update notification");

        assert_eq!(
            release_notification_version(&cache, &settings.load().ui, "1.0.0", false),
            None
        );
        assert_eq!(
            release_update_available(&cache, true, "1.0.0"),
            Some("2.0.0")
        );
    }
}
