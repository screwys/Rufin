use std::fs;
use std::path::{Path, PathBuf};

use app_identity::PROJECT_NAME;
use directories::ProjectDirs;

const STORE_DIRECTORY: &str = "store";
const STORE_FILE: &str = "rufin-store.sqlite";
const SETTINGS_FILE: &str = "settings.json";
const SECRETS_FILE: &str = "secrets.json";
const ARTWORK_DIRECTORY: &str = "covers";
const PLAYBACK_DIRECTORY: &str = "playback";
const DOWNLOADS_DIRECTORY: &str = "downloads";
const RELEASE_HISTORY_FILE: &str = "releases.json";

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("io.github", "screwys", PROJECT_NAME)
}

pub(crate) fn project_cache_dir() -> Option<PathBuf> {
    project_dirs().map(|dirs| dirs.cache_dir().to_path_buf())
}

pub(crate) fn config_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn cache_dir() -> PathBuf {
    project_cache_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn data_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn state_dir() -> PathBuf {
    project_dirs()
        .and_then(|dirs| dirs.state_dir().map(Path::to_path_buf))
        .unwrap_or_else(cache_dir)
}

pub(crate) fn settings_file() -> PathBuf {
    config_dir().join(SETTINGS_FILE)
}

pub(crate) fn secrets_file() -> PathBuf {
    config_dir().join(SECRETS_FILE)
}

pub(crate) fn store_file() -> PathBuf {
    cache_dir().join(STORE_DIRECTORY).join(STORE_FILE)
}

pub(crate) fn artwork_dir() -> PathBuf {
    cache_dir().join(ARTWORK_DIRECTORY)
}

pub(crate) fn playback_dir() -> PathBuf {
    cache_dir().join(PLAYBACK_DIRECTORY)
}

pub(crate) fn downloads_dir() -> PathBuf {
    data_dir().join(DOWNLOADS_DIRECTORY)
}

pub(crate) fn release_history_file() -> PathBuf {
    cache_dir().join(RELEASE_HISTORY_FILE)
}

pub(crate) fn prepare() -> Result<(), String> {
    for path in [
        store_file().parent().map(Path::to_path_buf),
        Some(artwork_dir()),
        Some(playback_dir()),
        Some(downloads_dir()),
        settings_file().parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        fs::create_dir_all(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}
