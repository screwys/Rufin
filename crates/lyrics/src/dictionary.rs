use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lindera::dictionary::load_fs_dictionary;

const DICTIONARY_DIRECTORY: &str = "ipadic-6.0.0";
const DICTIONARY_URL: &str =
    "https://github.com/lindera/lindera/releases/download/v6.0.0/lindera-ipadic-6.0.0.zip";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum JapaneseDictionaryStatus {
    #[default]
    Idle,
    Loading,
    Ready(PathBuf),
    Failed,
}

pub(crate) fn prepare_dictionary(directory: &Path) -> Result<PathBuf, String> {
    install_dictionary(directory, DICTIONARY_URL).map_err(|error| error.to_string())
}

fn install_dictionary(directory: &Path, url: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let destination = directory.join(DICTIONARY_DIRECTORY);
    if destination.is_dir() {
        load_fs_dictionary(&destination)?;
        return Ok(destination);
    }

    fs::create_dir_all(directory)?;
    // Keep incomplete downloads out of the installed directory, on the same filesystem
    // so publication is a rename. Dropping the temporary directory cleans up failures.
    let staging = tempfile::tempdir_in(directory)?;
    let mut archive = tempfile::tempfile_in(staging.path())?;
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .copy_to(&mut archive)?;
    zip::ZipArchive::new(archive)?.extract(staging.path())?;
    let extracted = staging.path().join("lindera-ipadic");
    // Validate and close the memory mappings before moving files (also on Windows).
    load_fs_dictionary(&extracted)?;
    fs::rename(extracted, &destination)?;
    Ok(destination)
}

#[cfg(test)]
pub(crate) fn test_dictionary() -> &'static Path {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            prepare_dictionary(directory.path()).expect("download Japanese dictionary");
            directory
        })
        .path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "downloads the pinned Lindera dictionary"]
    fn downloaded_dictionary_is_reused_without_network() {
        let directory = test_dictionary();
        let path = install_dictionary(directory, "http://127.0.0.1:0/unavailable").unwrap();
        assert_eq!(path, directory.join(DICTIONARY_DIRECTORY));
        assert!(path.join("NOTICE.txt").is_file());
        assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
    }

    #[test]
    fn failed_download_does_not_install_partial_dictionary() {
        let directory = tempfile::tempdir().unwrap();
        assert!(install_dictionary(directory.path(), "http://127.0.0.1:0/unavailable").is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
