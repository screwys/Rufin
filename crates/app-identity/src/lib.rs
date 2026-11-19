//! Rufin's application identity shared by runtime and platform integrations.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use directories::ProjectDirs;

pub const STABLE_APP_ID: &str = "app.rufin.rufin";
pub const STABLE_DISPLAY_NAME: &str = "Rufin";
pub const STABLE_PROJECT_NAME: &str = "Rufin";
pub const DEVELOPMENT_APP_ID: &str = "app.rufin.rufin.Devel";
pub const DEVELOPMENT_DISPLAY_NAME: &str = "Rufin (Development)";

#[cfg(feature = "development")]
pub const APP_ID: &str = DEVELOPMENT_APP_ID;
#[cfg(not(feature = "development"))]
pub const APP_ID: &str = STABLE_APP_ID;

/// The ID releases up to 0.18 had; their keychain items are named after it.
#[cfg(feature = "development")]
pub const PREVIOUS_APP_ID: &str = "io.github.screwys.Rufin.Devel";
#[cfg(not(feature = "development"))]
pub const PREVIOUS_APP_ID: &str = "io.github.screwys.Rufin";

#[cfg(feature = "development")]
pub const DISPLAY_NAME: &str = DEVELOPMENT_DISPLAY_NAME;
#[cfg(not(feature = "development"))]
pub const DISPLAY_NAME: &str = STABLE_DISPLAY_NAME;

#[cfg(feature = "development")]
pub const PROJECT_NAME: &str = "Rufin.Devel";
#[cfg(not(feature = "development"))]
pub const PROJECT_NAME: &str = STABLE_PROJECT_NAME;

/// Rufin's configuration, data and cache folders.
pub struct Folders {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub state: Option<PathBuf>,
}

/// The first call moves the folders releases up to 0.18 kept under the old ID. A folder that
/// cannot move stays in use where it is, and the next launch tries again.
pub fn folders() -> Option<&'static Folders> {
    static FOLDERS: OnceLock<Option<Folders>> = OnceLock::new();
    FOLDERS
        .get_or_init(|| {
            // macOS names an app's folders after its bundle ID, Linux in lowercase.
            let name = if cfg!(target_os = "macos") {
                APP_ID.to_owned()
            } else if cfg!(windows) {
                PROJECT_NAME.to_owned()
            } else {
                PROJECT_NAME.to_lowercase()
            };
            let current = ProjectDirs::from_path(PathBuf::from(name))?;
            let previous = ProjectDirs::from("io.github", "screwys", PROJECT_NAME);
            let folder = |current: &Path, previous: Option<&Path>| {
                previous.map_or_else(|| current.to_owned(), |previous| moved(previous, current))
            };
            Some(Folders {
                config: folder(
                    current.config_dir(),
                    previous.as_ref().map(ProjectDirs::config_dir),
                ),
                data: folder(
                    current.data_dir(),
                    previous.as_ref().map(ProjectDirs::data_dir),
                ),
                cache: folder(
                    current.cache_dir(),
                    previous.as_ref().map(ProjectDirs::cache_dir),
                ),
                state: current.state_dir().map(Path::to_path_buf),
            })
        })
        .as_ref()
}

/// Where a folder is after moving it from its previous place: there, unless it could not move.
fn moved(previous: &Path, current: &Path) -> PathBuf {
    if previous == current || !previous.exists() || current.exists() {
        return current.to_owned();
    }
    let renamed = current
        .parent()
        .is_none_or(|parent| std::fs::create_dir_all(parent).is_ok())
        && std::fs::rename(previous, current).is_ok();
    if !renamed {
        return previous.to_owned();
    }
    // Windows nested them under the developer's name: `screwys\Rufin\config`.
    if cfg!(windows) {
        for empty in previous.ancestors().skip(1).take(2) {
            let _ = std::fs::remove_dir(empty);
        }
    }
    current.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_previous_folder_moves_unless_it_cannot() {
        let home = tempfile::tempdir().unwrap();
        let previous = home.path().join("screwys").join("Rufin").join("config");
        let current = home.path().join("Rufin").join("config");
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join("rufin.toml"), "kept").unwrap();
        assert_eq!(moved(&previous, &current), current);
        assert_eq!(
            std::fs::read_to_string(current.join("rufin.toml")).unwrap(),
            "kept"
        );
        assert!(!previous.exists());

        // A current folder that already exists wins; the previous one is left alone.
        std::fs::create_dir_all(&previous).unwrap();
        assert_eq!(moved(&previous, &current), current);
        assert!(previous.exists());

        // A folder that cannot move stays in use.
        let blocked = home.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        assert_eq!(moved(&previous, &blocked.join("config")), previous);
    }
}
