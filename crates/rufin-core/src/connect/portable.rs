//! The portable file contains shared state only. Device identity and playback stay here.
use age::secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) enum ExchangeFolder {
    Local(PathBuf),
    Remote {
        source: Arc<sources::Source>,
        path: String,
    },
    Document(Box<sources::FileSource>),
}

pub(super) struct ExchangeFile {
    pub path: String,
    pub version: Option<String>,
}

impl ExchangeFile {
    pub fn name(&self) -> &str {
        sources::FileSource::file_name(&self.path)
    }
}

impl ExchangeFolder {
    pub fn new(owner: &super::ConnectOwner, destination: Destination) -> Result<Self, String> {
        Ok(match destination {
            Destination::Local { path } => Self::Local(path),
            Destination::Remote { source_id, path } => Self::Remote {
                source: owner.source.client(&source_id)?,
                path,
            },
            Destination::Document { uri } => {
                Self::Document(Box::new(sources::FileSource::document_profile(
                    sources::DocumentRoot::new(uri, "").map_err(error)?,
                )))
            }
        })
    }

    fn native(&self) -> Result<&sources::FileSource, String> {
        match self {
            Self::Remote { source, .. } => source.profile_file_source().map_err(error),
            Self::Document(source) => Ok(source),
            Self::Local(_) => unreachable!("Local exchange uses filesystem operations"),
        }
    }

    fn remote_path(&self, relative: &str) -> String {
        let Self::Remote { path, .. } = self else {
            return relative.into();
        };
        [path.trim_matches('/'), relative.trim_matches('/')]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    }

    pub async fn ensure(&self, relative: &str) -> Result<(), String> {
        match self {
            Self::Local(path) => tokio::fs::create_dir_all(path.join(relative))
                .await
                .map_err(error),
            _ => self
                .native()?
                .ensure_profile_folder(&self.remote_path(relative))
                .await
                .map_err(error),
        }
    }

    pub async fn visit<F, Fut>(&self, relative: &str, mut visit: F) -> Result<(), String>
    where
        F: FnMut(ExchangeFile) -> Fut + Send,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        match self {
            Self::Local(path) => {
                let mut directory = match tokio::fs::read_dir(path.join(relative)).await {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(failure) => return Err(error(failure)),
                };
                while let Some(entry) = directory.next_entry().await.map_err(error)? {
                    let name = entry.file_name();
                    let Some(name) = name
                        .to_str()
                        .filter(|name| name.ends_with(".rufin-connect"))
                    else {
                        continue;
                    };
                    let metadata = entry.metadata().await.map_err(error)?;
                    if metadata.is_file() {
                        visit(ExchangeFile {
                            path: if relative.is_empty() {
                                name.into()
                            } else {
                                format!("{relative}/{name}")
                            },
                            version: Some(format!(
                                "{:?}:{}",
                                metadata.modified().map_err(error)?,
                                metadata.len()
                            )),
                        })
                        .await?;
                    }
                }
                Ok(())
            }
            _ => match self
                .native()?
                .visit_files(
                    &self.remote_path(relative),
                    ".rufin-connect",
                    |(path, version)| {
                        let next = visit(ExchangeFile { path, version });
                        async move { next.await.map_err(sources::SourceError::Other) }
                    },
                )
                .await
            {
                Ok(()) | Err(sources::SourceError::NotFound) => Ok(()),
                Err(failure) => Err(error(failure)),
            },
        }
    }

    pub async fn read(
        &self,
        file: &ExchangeFile,
        target: &Path,
    ) -> Result<Option<PathBuf>, String> {
        match self {
            Self::Local(path) => Ok(Some(path.join(&file.path))),
            _ => self
                .native()?
                .copy_file(&file.path, target)
                .await
                .map(|file| file.map(|_| target.into()))
                .map_err(error),
        }
    }

    pub async fn read_named(
        &self,
        relative: &str,
        target: &Path,
    ) -> Result<Option<PathBuf>, String> {
        if let Self::Local(path) = self {
            let path = path.join(relative);
            return tokio::fs::try_exists(&path)
                .await
                .map(|exists| exists.then_some(path))
                .map_err(error);
        }
        self.read(
            &ExchangeFile {
                path: self.remote_path(relative),
                version: None,
            },
            target,
        )
        .await
    }

    pub async fn write(
        &self,
        relative: &str,
        output: tempfile::NamedTempFile,
    ) -> Result<ExchangeFile, String> {
        let (path, version) = match self {
            Self::Local(path) => {
                let path = path.join(relative);
                install(output, path.clone()).await?;
                let metadata = tokio::fs::metadata(path).await.map_err(error)?;
                (
                    relative.into(),
                    Some(format!(
                        "{:?}:{}",
                        metadata.modified().map_err(error)?,
                        metadata.len()
                    )),
                )
            }
            _ => {
                let path = self.remote_path(relative);
                let source = self.native()?;
                source
                    .write_profile_file(&path, output, Some(None))
                    .await
                    .map_err(error)?;
                let version = source.profile_file_version(&path).await.map_err(error)?;
                (path, version)
            }
        };
        Ok(ExchangeFile { path, version })
    }
}

const MAGIC: &[u8; 8] = b"RUFINCX2";
const UNCOMPRESSED_MAGIC: &[u8; 8] = b"RUFINCX1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Destination {
    Local {
        path: PathBuf,
    },
    Document {
        uri: String,
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
        matches!(self, Self::Remote { .. })
    }
    pub fn source_id(&self) -> Option<&sources::SourceId> {
        match self {
            Self::Remote { source_id, .. } => Some(source_id),
            Self::Local { .. } | Self::Document { .. } => None,
        }
    }
}

pub fn new_key() -> String {
    age::x25519::Identity::generate()
        .to_string()
        .expose_secret()
        .to_owned()
}

pub fn encrypt(
    output: File,
    profile: &str,
    key: &str,
) -> Result<age::stream::StreamWriter<File>, String> {
    let identity: age::x25519::Identity = key.parse().map_err(error)?;
    let recipient = identity.to_public();
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(error)?;
    let mut stream = encryptor.wrap_output(output).map_err(error)?;
    stream.write_all(MAGIC).map_err(error)?;
    let length: u16 = profile.len().try_into().map_err(error)?;
    stream.write_all(&length.to_le_bytes()).map_err(error)?;
    stream.write_all(profile.as_bytes()).map_err(error)?;
    Ok(stream)
}

/// Retain encrypted input so import and comparison can read it without plaintext staging.
pub struct Snapshot {
    file: File,
    identities: Vec<age::x25519::Identity>,
    profile: String,
}

impl Snapshot {
    pub fn reader(&self) -> Result<age::stream::StreamReader<File>, String> {
        let mut file = self.file.try_clone().map_err(error)?;
        file.rewind().map_err(error)?;
        let (profile, reader) = open_snapshot(file, &self.identities)?;
        if profile != self.profile {
            return Err("The Connect file belongs to another profile".into());
        }
        Ok(reader)
    }
}

/// Authenticate the complete input before returning a snapshot for application.
pub fn decrypt(input: &Path, keys: &[String]) -> Result<(String, Snapshot), String> {
    let identities: Vec<age::x25519::Identity> = keys
        .iter()
        .map(|key| key.parse::<age::x25519::Identity>().map_err(error))
        .collect::<Result<Vec<_>, _>>()?;
    let file = File::open(input).map_err(error)?;
    let (profile, mut stream) = open_snapshot(file.try_clone().map_err(error)?, &identities)?;
    std::io::copy(&mut stream, &mut std::io::sink()).map_err(error)?;
    Ok((
        profile.clone(),
        Snapshot {
            file,
            identities,
            profile,
        },
    ))
}

fn open_snapshot(
    input: File,
    identities: &[age::x25519::Identity],
) -> Result<(String, age::stream::StreamReader<File>), String> {
    let decryptor = age::Decryptor::new(input).map_err(error)?;
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
    Ok((profile, stream))
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
        let mut compressed = flate2::write::GzEncoder::new(
            File::create(&snapshot).unwrap(),
            flate2::Compression::default(),
        );
        compressed.write_all(&contents).unwrap();
        compressed.finish().unwrap();
        let key = new_key();
        let mut output = encrypt(File::create(&encrypted).unwrap(), "profile", &key).unwrap();
        std::io::copy(&mut File::open(&snapshot).unwrap(), &mut output).unwrap();
        output.finish().unwrap();
        let ciphertext = std::fs::read(&encrypted).unwrap();
        assert!(ciphertext.len() < contents.len() / 10);
        assert!(!ciphertext.windows(11).any(|bytes| bytes == b"credentials"));
        let (profile, staged) = decrypt(&encrypted, std::slice::from_ref(&key)).unwrap();
        assert_eq!(profile, "profile");
        let mut restored = Vec::new();
        flate2::read::MultiGzDecoder::new(staged.reader().unwrap())
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, contents);
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
        let mut restored = String::new();
        staged
            .reader()
            .unwrap()
            .read_to_string(&mut restored)
            .unwrap();
        assert_eq!(restored, "1\nEND\n");
    }
    #[test]
    fn rotated_keys_read_existing_exports_and_rewrite_for_current_members() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("state");
        let destination = dir.path().join("profile");
        let mut compressed = flate2::write::GzEncoder::new(
            File::create(&snapshot).unwrap(),
            flate2::Compression::default(),
        );
        compressed.write_all(b"1\nEND\n").unwrap();
        compressed.finish().unwrap();
        let old = new_key();
        let intermediate = new_key();
        let current = new_key();
        let mut output = encrypt(File::create(&destination).unwrap(), "profile", &old).unwrap();
        std::io::copy(&mut File::open(&snapshot).unwrap(), &mut output).unwrap();
        output.finish().unwrap();
        let keys = vec![current.clone(), old.clone(), intermediate];
        let (profile, staged) = decrypt(&destination, &keys).unwrap();
        let rewritten = dir.path().join("rewritten");
        let mut output = encrypt(File::create(&rewritten).unwrap(), &profile, &current).unwrap();
        std::io::copy(&mut staged.reader().unwrap(), &mut output).unwrap();
        output.finish().unwrap();
        assert!(decrypt(&rewritten, &[old]).is_err());
        assert!(decrypt(&rewritten, &[current]).is_ok());
    }
}
