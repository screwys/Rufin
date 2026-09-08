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
    Downloading,
    Ready(PathBuf),
    Failed,
}

pub fn prepare_dictionary(directory: &Path, downloading: impl FnOnce()) -> Result<PathBuf, String> {
    install_dictionary(directory, DICTIONARY_URL, downloading).map_err(|error| error.to_string())
}

fn install_dictionary(
    directory: &Path,
    url: &str,
    downloading: impl FnOnce(),
) -> Result<PathBuf, Box<dyn std::error::Error>> {
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
    downloading();
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
mod tests {
    use super::*;

    #[test]
    fn failed_download_does_not_install_partial_dictionary() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            install_dictionary(directory.path(), "http://127.0.0.1:0/unavailable", || {}).is_err()
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
