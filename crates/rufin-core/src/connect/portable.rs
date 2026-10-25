//! The portable file contains shared state only. Device identity and playback stay here.
use age::secrecy::ExposeSecret;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

pub fn new_key() -> String {
    age::x25519::Identity::generate()
        .to_string()
        .expose_secret()
        .to_owned()
}

pub async fn install(input: tempfile::NamedTempFile, destination: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        input.as_file().sync_all().map_err(error)?;
        let input = match input.persist(&destination) {
            Ok(_) => return Ok(()),
            Err(cause) if cause.error.kind() == std::io::ErrorKind::CrossesDevices => cause.file,
            Err(cause) => return Err(error(cause)),
        };
        let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        std::io::copy(&mut File::open(input.path()).map_err(error)?, &mut pending)
            .map_err(error)?;
        pending.as_file().sync_all().map_err(error)?;
        pending.persist(destination).map_err(error)?;
        Ok(())
    })
    .await
    .map_err(error)?
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
