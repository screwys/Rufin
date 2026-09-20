//! The portable file contains shared state only. Device identity and playback stay here.
use age::secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"RUFINCX2";
const UNCOMPRESSED_MAGIC: &[u8; 8] = b"RUFINCX1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    Local {
        path: PathBuf,
    },
    // Keep the existing file format; the saved connection owns its protocol.
    #[serde(rename = "web_dav", alias = "smb")]
    Remote {
        source_id: sources::SourceId,
        path: String,
    },
}

impl Destination {
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local { .. })
    }
    pub fn source_id(&self) -> Option<&sources::SourceId> {
        match self {
            Self::Remote { source_id, .. } => Some(source_id),
            Self::Local { .. } => None,
        }
    }
}

pub fn new_key() -> String {
    age::x25519::Identity::generate()
        .to_string()
        .expose_secret()
        .to_owned()
}

pub fn encrypt(snapshot: &Path, output: &Path, profile: &str, key: &str) -> Result<(), String> {
    let identity: age::x25519::Identity = key.parse().map_err(error)?;
    let recipient = identity.to_public();
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(error)?;
    let file = File::create(output).map_err(error)?;
    let mut stream = encryptor.wrap_output(file).map_err(error)?;
    stream.write_all(MAGIC).map_err(error)?;
    let length: u16 = profile.len().try_into().map_err(error)?;
    stream.write_all(&length.to_le_bytes()).map_err(error)?;
    stream.write_all(profile.as_bytes()).map_err(error)?;
    let mut compressed = flate2::write::GzEncoder::new(stream, flate2::Compression::default());
    std::io::copy(&mut File::open(snapshot).map_err(error)?, &mut compressed).map_err(error)?;
    let stream = compressed.finish().map_err(error)?;
    stream.finish().map_err(error)?.sync_all().map_err(error)
}

/// Authenticate the complete input before returning a snapshot for application.
pub fn decrypt(input: &Path, keys: &[String]) -> Result<(String, tempfile::NamedTempFile), String> {
    let identities = keys
        .iter()
        .map(|key| key.parse::<age::x25519::Identity>().map_err(error))
        .collect::<Result<Vec<_>, _>>()?;
    let decryptor = age::Decryptor::new(File::open(input).map_err(error)?).map_err(error)?;
    let mut stream = decryptor
        .decrypt(
            identities
                .iter()
                .map(|identity| identity as &dyn age::Identity),
        )
        .map_err(error)?;
    let mut magic = [0u8; 8];
    stream.read_exact(&mut magic).map_err(error)?;
    if &magic != MAGIC && &magic != UNCOMPRESSED_MAGIC {
        return Err(super::error(rufin_connect::profile::INCOMPATIBLE_PROFILE));
    }
    let mut length = [0u8; 2];
    stream.read_exact(&mut length).map_err(error)?;
    let mut profile = vec![0; usize::from(u16::from_le_bytes(length))];
    stream.read_exact(&mut profile).map_err(error)?;
    let profile = String::from_utf8(profile).map_err(error)?;
    let mut staged = tempfile::NamedTempFile::new().map_err(error)?;
    if &magic == MAGIC {
        std::io::copy(&mut flate2::read::MultiGzDecoder::new(stream), &mut staged)
            .map_err(error)?;
    } else {
        std::io::copy(&mut stream, &mut staged).map_err(error)?;
    }
    staged.as_file().sync_all().map_err(error)?;
    Ok((profile, staged))
}

pub async fn install(input: tempfile::NamedTempFile, destination: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_file_authenticates_before_exposing_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("snapshot");
        let encrypted = dir.path().join("profile.rufin-connect");
        let contents = b"shared state including credentials\n".repeat(10_000);
        std::fs::write(&snapshot, &contents).unwrap();
        let key = new_key();
        encrypt(&snapshot, &encrypted, "profile", &key).unwrap();
        let ciphertext = std::fs::read(&encrypted).unwrap();
        assert!(ciphertext.len() < contents.len() / 10);
        assert!(!ciphertext.windows(11).any(|bytes| bytes == b"credentials"));
        let (profile, staged) = decrypt(&encrypted, std::slice::from_ref(&key)).unwrap();
        assert_eq!(profile, "profile");
        assert_eq!(
            std::fs::read(staged.path()).unwrap(),
            std::fs::read(&snapshot).unwrap()
        );
        assert!(decrypt(&encrypted, &[new_key()]).is_err());
        std::fs::write(&encrypted, &ciphertext[..ciphertext.len() - 1]).unwrap();
        assert!(decrypt(&encrypted, std::slice::from_ref(&key)).is_err());
    }

    #[test]
    fn reads_uncompressed_profile_files() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("profile.rufin-connect");
        let key = new_key();
        let identity: age::x25519::Identity = key.parse().unwrap();
        let recipient = identity.to_public();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .unwrap();
        let mut stream = encryptor
            .wrap_output(File::create(&encrypted).unwrap())
            .unwrap();
        stream.write_all(UNCOMPRESSED_MAGIC).unwrap();
        stream.write_all(&7u16.to_le_bytes()).unwrap();
        stream.write_all(b"profile1\nEND\n").unwrap();
        stream.finish().unwrap();
        let (profile, staged) = decrypt(&encrypted, &[key]).unwrap();
        assert_eq!(profile, "profile");
        assert_eq!(std::fs::read(staged.path()).unwrap(), b"1\nEND\n");
    }
    #[test]
    fn rotated_keys_read_existing_exports_and_rewrite_for_current_members() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("state");
        let destination = dir.path().join("profile");
        std::fs::write(&snapshot, b"1\nEND\n").unwrap();
        let old = new_key();
        let intermediate = new_key();
        let current = new_key();
        encrypt(&snapshot, &destination, "profile", &old).unwrap();
        let keys = vec![current.clone(), old.clone(), intermediate];
        let (profile, staged) = decrypt(&destination, &keys).unwrap();
        encrypt(staged.path(), &destination, &profile, &current).unwrap();
        assert!(decrypt(&destination, &[old]).is_err());
        assert!(decrypt(&destination, &[current]).is_ok());
    }
}
