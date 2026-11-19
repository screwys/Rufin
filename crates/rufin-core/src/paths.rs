use std::fs;
use std::path::{Path, PathBuf};

pub fn project_cache_dir() -> Option<PathBuf> {
    app_identity::folders().map(|folders| folders.cache.clone())
}

fn folder(pick: impl FnOnce(&app_identity::Folders) -> PathBuf) -> PathBuf {
    app_identity::folders().map_or_else(|| PathBuf::from("."), pick)
}

pub fn state_dir() -> PathBuf {
    folder(|folders| {
        folders
            .state
            .clone()
            .unwrap_or_else(|| folders.cache.clone())
    })
}

pub fn roots() -> Paths {
    Paths {
        config: folder(|folders| folders.config.clone()),
        cache: folder(|folders| folders.cache.clone()),
        data: folder(|folders| folders.data.clone()),
        state: state_dir(),
    }
}

const STORE_DIRECTORY: &str = "store";
const STORE_FILE: &str = "rufin-library.sqlite";
// Releases up to 0.16 kept their Store here; it is imported once and left for a downgrade.
const RELEASED_STORE_FILE: &str = "rufin-store.sqlite";
const SETTINGS_FILE: &str = "rufin.toml";
const ARTWORK_DIRECTORY: &str = "covers";
const PLAYBACK_DIRECTORY: &str = "playback";
const DOWNLOADS_DIRECTORY: &str = "downloads";
const RELEASE_HISTORY_FILE: &str = "releases.json";

#[derive(Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
}
impl Paths {
    pub fn config_dir(&self) -> PathBuf {
        self.config.clone()
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.cache.clone()
    }
    pub fn data_dir(&self) -> PathBuf {
        self.data.clone()
    }
    pub fn state_dir(&self) -> PathBuf {
        self.state.clone()
    }
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir().join(SETTINGS_FILE)
    }

    pub fn store_file(&self) -> PathBuf {
        self.data_dir().join(STORE_DIRECTORY).join(STORE_FILE)
    }

    pub fn released_store_file(&self) -> PathBuf {
        self.data_dir()
            .join(STORE_DIRECTORY)
            .join(RELEASED_STORE_FILE)
    }

    pub fn catalog_file(&self) -> PathBuf {
        self.cache_dir()
            .join(STORE_DIRECTORY)
            .join("rufin-catalog.sqlite")
    }

    pub fn artwork_dir(&self) -> PathBuf {
        self.cache_dir().join(ARTWORK_DIRECTORY)
    }

    pub fn playback_dir(&self) -> PathBuf {
        self.cache_dir().join(PLAYBACK_DIRECTORY)
    }

    pub fn connect_dir(&self) -> PathBuf {
        self.data_dir().join("connect")
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.data_dir().join(DOWNLOADS_DIRECTORY)
    }

    pub fn release_history_file(&self) -> PathBuf {
        self.cache_dir().join(RELEASE_HISTORY_FILE)
    }

    pub fn prepare(&self) -> Result<(), String> {
        for path in [
            self.store_file().parent().map(Path::to_path_buf),
            Some(self.artwork_dir()),
            Some(self.playback_dir()),
            Some(self.downloads_dir()),
            self.settings_file().parent().map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten()
        {
            fs::create_dir_all(path).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
