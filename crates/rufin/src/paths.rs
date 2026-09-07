use std::path::{Path, PathBuf};

use app_identity::PROJECT_NAME;
use directories::ProjectDirs;

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

pub(crate) fn roots() -> rufin_core::paths::Paths {
    rufin_core::paths::Paths {
        config: config_dir(),
        cache: cache_dir(),
        data: data_dir(),
        state: state_dir(),
    }
}
