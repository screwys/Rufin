//! SMB protocol operations. Library identity and file interpretation stay in Sources/Library.

mod shares;

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ::smb as protocol;
use protocol::{
    Client, ClientConfig, ConnectionConfig, FileAccessMask, FileCreateArgs, ReadAt, UncPath,
    WriteAt, connection::EncryptionMode,
};
use smb_fscc::{
    FileAllInformation, FileDispositionInformation, FileFsVolumeInformation,
    FileIdFullDirectoryInformation, FileNotifyInformation, FileRenameInformation,
};
use smb_msg::{CreateOptions, NotifyFilter, Status};

use crate::{SourceError, SourceResult};

pub(crate) struct SmbClient {
    client: Option<Arc<Client>>,
    root: UncPath,
    failed: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
}

pub(crate) struct File {
    handle: Option<Arc<protocol::File>>,
    failed: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
}

impl File {
    fn handle(&self) -> &Arc<protocol::File> {
        self.handle.as_ref().expect("live SMB file")
    }

    pub async fn flush(&self) -> SourceResult<()> {
        let handle = Arc::clone(self.handle());
        let failed = Arc::clone(&self.failed);
        blocking(move || {
            handle.flush().map_err(|error| {
                failed.store(true, Ordering::Release);
                SourceError::Network(error.to_string())
            })
        })
        .await
    }

    pub async fn close(mut self) -> SourceResult<()> {
        let handle = self.handle.take().expect("live SMB file");
        let failed = Arc::clone(&self.failed);
        blocking(move || handle.close().map_err(|error| record_error(&failed, error))).await
    }
}

// SMB's multithreaded client owns socket dispatch. Keep its blocking waits off
// Tokio workers so scanning, playback and source cancellation remain independent.
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> SourceResult<T> + Send + 'static,
) -> SourceResult<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

/// Facts from one observed entry, not an application track identity.
#[derive(Debug)]
pub(crate) struct Entry {
    pub path: String,
    pub directory: bool,
    pub size: u64,
    pub revision: String,
    pub native_id: Option<String>,
}

impl SmbClient {
    pub async fn connect(
        host: &str,
        port: u16,
        share: &str,
        username: &str,
        password: String,
        guest: bool,
        encryption_required: bool,
    ) -> SourceResult<Self> {
        let host = host.to_string();
        let share = share.to_string();
        let username = username.to_string();
        let runtime = tokio::runtime::Handle::current();
        blocking(move || {
            let host = server_address(&host, port);
            let root = UncPath::from_str(&format!(r"\\{host}\{share}"))
                .map_err(|error| SourceError::InvalidConfig(error.to_string()))?;
            let client = make_client(port, guest, encryption_required);
            client
                .share_connect(
                    &root,
                    if guest { "guest" } else { &username },
                    if guest { String::new() } else { password },
                )
                .map_err(error)?;
            let source = Self {
                client: Some(Arc::new(client)),
                root,
                failed: Arc::new(AtomicBool::new(false)),
                runtime,
            };
            let directory = directory(source.client(), &source.root, &source.failed)?;
            directory.close().map_err(error)?;
            Ok(source)
        })
        .await
    }

    fn client(&self) -> &Arc<Client> {
        self.client.as_ref().expect("live SMB connection")
    }

    pub fn is_disconnected(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub async fn with_root(mut self, root: String) -> SourceResult<Self> {
        self.root = self.path(&root)?;
        if !self.stat("").await?.directory {
            return Err(SourceError::InvalidRequest(
                "SMB source root is not a directory",
            ));
        }
        Ok(self)
    }

    fn path(&self, relative: &str) -> SourceResult<UncPath> {
        path_name(relative)?;
        Ok(self
            .root
            .clone()
            .with_add_path(&relative.replace('/', "\\")))
    }

    fn error(&self, error: protocol::Error) -> SourceError {
        record_error(&self.failed, error)
    }

    pub async fn open_read(&self, path: &str) -> SourceResult<(File, Entry)> {
        let client = Arc::clone(self.client());
        let runtime = self.runtime.clone();
        let failed = Arc::clone(&self.failed);
        let target = self.path(path)?;
        let path = path.to_string();
        blocking(move || {
            let resource = client
                .create_file(&target, &read_args())
                .map_err(|error| record_error(&failed, error))?;
            match resource {
                protocol::Resource::File(file) => {
                    let entry = match facts(&file, &path) {
                        Ok(entry) => entry,
                        Err(error) => {
                            let _ = file.close();
                            return Err(error);
                        }
                    };
                    Ok((
                        File {
                            handle: Some(Arc::new(file)),
                            failed,
                            runtime,
                        },
                        entry,
                    ))
                }
                other => {
                    handle(&other)
                        .close()
                        .map_err(|error| record_error(&failed, error))?;
                    Err(SourceError::InvalidRequest("SMB entry is not a file"))
                }
            }
        })
        .await
    }

    pub async fn stat(&self, path: &str) -> SourceResult<Entry> {
        let client = Arc::clone(self.client());
        let failed = Arc::clone(&self.failed);
        let target = self.path(path)?;
        let path = path.to_string();
        blocking(move || {
            let resource = client
                .create_file(&target, &read_args())
                .map_err(|error| record_error(&failed, error))?;
            let entry = facts(handle(&resource), &path);
            let closed = handle(&resource).close();
            let entry = entry?;
            closed.map_err(|error| record_error(&failed, error))?;
            Ok(entry)
        })
        .await
    }

    pub async fn list<F: std::future::Future<Output = SourceResult<()>>>(
        &self,
        path: &str,
        mut accept: impl FnMut(Entry) -> F,
    ) -> SourceResult<()> {
        let client = Arc::clone(self.client());
        let failed = Arc::clone(&self.failed);
        let target = self.path(path)?;
        let path = path.to_string();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let work = tokio::task::spawn_blocking(move || {
            let directory = directory(&client, &target, &failed)?;
            let result = (|| {
                let volume = directory
                    .query_fs_info::<FileFsVolumeInformation>()
                    .map_err(|error| record_error(&failed, error))?
                    .volume_serial_number;
                let entries = directory
                    .query::<FileIdFullDirectoryInformation>("*")
                    .map_err(|error| record_error(&failed, error))?;
                for entry in entries {
                    let entry = entry.map_err(|error| record_error(&failed, error))?;
                    let name = entry.file_name.to_string();
                    if matches!(name.as_str(), "." | "..") {
                        continue;
                    }
                    let child = if path.is_empty() {
                        name
                    } else {
                        format!("{path}/{name}")
                    };
                    path_name(&child)?;
                    sender
                        .blocking_send(Entry {
                            path: child,
                            directory: entry.file_attributes.directory(),
                            size: entry.end_of_file,
                            revision: format!(
                                "{}:{}:{}",
                                *entry.last_write_time, *entry.change_time, entry.end_of_file
                            ),
                            native_id: (entry.file_id != 0)
                                .then(|| format!("{volume}:{}", entry.file_id)),
                        })
                        .map_err(|_| SourceError::Cancelled)?;
                }
                Ok(())
            })();
            let closed = directory
                .close()
                .map_err(|error| record_error(&failed, error));
            result.and(closed)
        });
        while let Some(entry) = receiver.recv().await {
            accept(entry).await?;
        }
        work.await
            .map_err(|error| SourceError::Other(error.to_string()))?
    }

    /// `false` means this server explicitly does not implement change notifications.
    pub async fn watch(
        &self,
        mut changed: impl FnMut(FileNotifyInformation) -> bool,
    ) -> SourceResult<bool> {
        let client = Arc::clone(self.client());
        let failed = Arc::clone(&self.failed);
        let root = self.root.clone();
        let runtime = self.runtime.clone();
        let watched = blocking(move || {
            Ok(WatchDirectory {
                directory: Some(Arc::new(directory(&client, &root, &failed)?)),
                runtime,
            })
        })
        .await?;
        let directory = Arc::clone(watched.directory.as_ref().expect("watched SMB directory"));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let work = tokio::task::spawn_blocking(move || {
            let filter = NotifyFilter::new()
                .with_file_name(true)
                .with_dir_name(true)
                .with_last_write(true)
                .with_size(true);
            while !sender.is_closed() {
                match directory.watch(filter, true) {
                    Ok(changes) => {
                        for change in changes {
                            if sender.blocking_send(Ok(change)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error));
                        return;
                    }
                }
            }
        });
        while let Some(change) = receiver.recv().await {
            match change {
                Ok(change) => {
                    if !changed(change) {
                        return Ok(true);
                    }
                }
                Err(
                    protocol::Error::UnexpectedMessageStatus(status)
                    | protocol::Error::ReceivedErrorMessage(status, _),
                ) if status == Status::NotSupported as u32
                    || status == Status::NotImplemented as u32 =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(self.error(error)),
            }
        }
        work.await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(true)
    }

    pub async fn read(file: &File, offset: u64, length: usize) -> SourceResult<Vec<u8>> {
        let handle = Arc::clone(file.handle());
        let failed = Arc::clone(&file.failed);
        blocking(move || {
            let mut buffer = vec![0; length.min(65536)];
            let count = handle
                .read_at(&mut buffer, offset)
                .map_err(|error| record_error(&failed, error))?;
            buffer.truncate(count);
            Ok(buffer)
        })
        .await
    }

    pub async fn create(&self, path: &str) -> SourceResult<File> {
        let client = Arc::clone(self.client());
        let runtime = self.runtime.clone();
        let failed = Arc::clone(&self.failed);
        let target = self.path(path)?;
        blocking(move || {
            let args = FileCreateArgs::make_create_new(
                Default::default(),
                CreateOptions::new().with_non_directory_file(true),
            );
            let resource = client
                .create_file(&target, &args)
                .map_err(|error| record_error(&failed, error))?;
            match resource {
                protocol::Resource::File(file) => Ok(File {
                    handle: Some(Arc::new(file)),
                    failed,
                    runtime,
                }),
                other => {
                    handle(&other)
                        .close()
                        .map_err(|error| record_error(&failed, error))?;
                    Err(SourceError::InvalidRequest("SMB entry is not a file"))
                }
            }
        })
        .await
    }

    pub async fn write(file: &File, offset: u64, bytes: &[u8]) -> SourceResult<usize> {
        let handle = Arc::clone(file.handle());
        let failed = Arc::clone(&file.failed);
        let bytes = bytes[..bytes.len().min(65536)].to_vec();
        blocking(move || {
            handle
                .write_at(&bytes, offset)
                .map_err(|error| record_error(&failed, error))
        })
        .await
    }

    pub async fn rename(&self, from: &str, to: &str, overwrite: bool) -> SourceResult<()> {
        let client = Arc::clone(self.client());
        let failed = Arc::clone(&self.failed);
        let from_path = self.path(from)?;
        let to_path = self.path(to)?;
        blocking(move || {
            let destination = to_path;
            let file = client
                .create_file(
                    &from_path,
                    &FileCreateArgs::make_open_existing(FileAccessMask::new().with_delete(true)),
                )
                .map_err(|error| record_error(&failed, error))?;
            let renamed = handle(&file).set_info(FileRenameInformation {
                replace_if_exists: overwrite.into(),
                root_directory: 0,
                file_name: destination.path().unwrap_or("").into(),
            });
            let closed = handle(&file).close();
            renamed
                .and(closed)
                .map_err(|error| record_error(&failed, error))
        })
        .await
    }

    pub async fn remove(&self, path: &str) -> SourceResult<()> {
        let client = Arc::clone(self.client());
        let failed = Arc::clone(&self.failed);
        let target = self.path(path)?;
        blocking(move || {
            let file = client
                .create_file(
                    &target,
                    &FileCreateArgs::make_open_existing(FileAccessMask::new().with_delete(true)),
                )
                .map_err(|error| record_error(&failed, error))?;
            let removed = handle(&file).set_info(FileDispositionInformation::default());
            let closed = handle(&file).close();
            removed
                .and(closed)
                .map_err(|error| record_error(&failed, error))
        })
        .await
    }
}

impl Drop for SmbClient {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            // File/directory handles retain their protocol session until they close.
            self.runtime.spawn_blocking(move || drop(client));
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.runtime.spawn_blocking(move || {
                let _ = handle.close();
            });
        }
    }
}

fn directory(
    client: &Client,
    path: &UncPath,
    failed: &AtomicBool,
) -> SourceResult<protocol::Directory> {
    let resource = client
        .create_file(path, &read_args())
        .map_err(|error| record_error(failed, error))?;
    match resource {
        protocol::Resource::Directory(directory) => Ok(directory),
        other => {
            handle(&other)
                .close()
                .map_err(|error| record_error(failed, error))?;
            Err(SourceError::InvalidRequest("SMB entry is not a directory"))
        }
    }
}

struct WatchDirectory {
    directory: Option<Arc<protocol::Directory>>,
    runtime: tokio::runtime::Handle,
}
impl Drop for WatchDirectory {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            self.runtime.spawn_blocking(move || {
                let _ = directory.close();
            });
        }
    }
}

fn read_args() -> FileCreateArgs {
    FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true))
}

fn error(error: protocol::Error) -> SourceError {
    match error {
        protocol::Error::UnexpectedMessageStatus(status)
        | protocol::Error::ReceivedErrorMessage(status, _)
            if status == Status::LogonFailure as u32 =>
        {
            SourceError::Auth("SMB credentials were rejected".into())
        }
        protocol::Error::UnexpectedMessageStatus(status)
        | protocol::Error::ReceivedErrorMessage(status, _)
            if status == Status::ObjectNameNotFound as u32
                || status == Status::ObjectPathNotFound as u32 =>
        {
            SourceError::NotFound
        }
        protocol::Error::UnexpectedMessageStatus(status)
        | protocol::Error::ReceivedErrorMessage(status, _)
            if status == Status::AccessDenied as u32 =>
        {
            SourceError::Other("SMB access denied".into())
        }
        other => SourceError::Network(other.to_string()),
    }
}

fn handle(resource: &protocol::Resource) -> &protocol::resource::ResourceHandle {
    match resource {
        protocol::Resource::File(file) => file,
        protocol::Resource::Directory(directory) => directory,
        protocol::Resource::Pipe(pipe) => pipe,
    }
}

fn facts(file: &protocol::resource::ResourceHandle, path: &str) -> SourceResult<Entry> {
    let volume = file
        .query_fs_info::<FileFsVolumeInformation>()
        .map_err(error)?;
    let info = file.query_info::<FileAllInformation>().map_err(error)?;
    Ok(Entry {
        path: path.to_string(),
        directory: info.basic.file_attributes.directory(),
        size: info.standard.end_of_file,
        revision: format!(
            "{}:{}:{}",
            *info.basic.last_write_time, *info.basic.change_time, info.standard.end_of_file
        ),
        native_id: (info.internal.index_number != 0).then(|| {
            format!(
                "{}:{}",
                volume.volume_serial_number, info.internal.index_number
            )
        }),
    })
}

fn path_name(path: &str) -> SourceResult<()> {
    if path.contains(['\\', '\0'])
        || path.starts_with('/')
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(SourceError::InvalidRequest("path is outside the SMB share"));
    }
    Ok(())
}

fn record_error(failed: &AtomicBool, failure: protocol::Error) -> SourceError {
    let error = error(failure);
    if matches!(error, SourceError::Network(_)) {
        failed.store(true, Ordering::Release);
    }
    error
}

fn server_address(host: &str, port: u16) -> String {
    // The SMB client's address parser needs a port when the host contains colons.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(address) = literal.parse::<std::net::Ipv6Addr>() {
        format!("[{address}]:{port}")
    } else {
        host.into()
    }
}

fn make_client(port: u16, guest: bool, encryption_required: bool) -> Client {
    Client::new(ClientConfig {
        connection: ConnectionConfig {
            port: Some(port),
            timeout: Some(Duration::from_secs(20)),
            allow_unsigned_guest_access: guest,
            encryption_mode: if encryption_required {
                EncryptionMode::Required
            } else {
                EncryptionMode::Allowed
            },
            ..Default::default()
        },
        ..Default::default()
    })
}

pub async fn list_smb_shares(
    settings: crate::FileSourceSettings,
    credentials: crate::FileCredentials,
) -> SourceResult<Vec<(String, String)>> {
    blocking(move || {
        let mut url = url::Url::parse(&settings.url)
            .map_err(|e| SourceError::InvalidConfig(e.to_string()))?;
        if url.scheme() != "smb" || !url.username().is_empty() || url.password().is_some() {
            return Err(SourceError::InvalidConfig(
                "Enter an SMB server address and separate credentials".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or(SourceError::InvalidRequest(
                "SMB server address has no host",
            ))?
            .to_string();
        let host = server_address(&host, url.port().unwrap_or(445));
        let guest = settings.authentication == crate::FileAuthentication::Anonymous;
        let username = if guest {
            "guest".to_string()
        } else if settings.domain.is_empty() {
            settings.username
        } else {
            format!("{}\\{}", settings.domain, settings.username)
        };
        let client = make_client(
            url.port().unwrap_or(445),
            guest,
            settings.require_smb_encryption,
        );
        client
            .share_connect(
                &UncPath::ipc_share(&host).map_err(error)?,
                &username,
                if guest {
                    String::new()
                } else {
                    credentials.secret
                },
            )
            .map_err(error)?;
        let listed = shares::list(&client, &host);
        let _ = client.close();
        let mut shares = Vec::new();
        for name in listed? {
            url.set_path("/");
            url.path_segments_mut()
                .map_err(|_| SourceError::NotFound)?
                .pop_if_empty()
                .push(&name)
                .push("");
            shares.push((name, url.to_string()));
        }
        shares.sort();
        Ok(shares)
    })
    .await
}
