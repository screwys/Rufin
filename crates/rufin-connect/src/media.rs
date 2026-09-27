//! Transfer existing media files using Iroh's verified range protocol.
use crate::network::ConnectNetwork;
use anyhow::{Context, Result, bail};
use bao_tree::{
    BaoTree, ChunkNum, ChunkRanges,
    io::{
        BaoContentItem,
        outboard::{PreOrderMemOutboard, PreOrderOutboard},
        sync::CreateOutboard,
    },
};
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, SendStream},
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_blobs::{
    Hash,
    get::fsm,
    protocol::{GetRequest, Request},
    store::IROH_BLOCK_SIZE,
};
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::UNIX_EPOCH,
};
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Snapshot {
    file: tempfile::TempPath,
    outboard: PreOrderMemOutboard<Arc<[u8]>>,
}

#[derive(Clone, Debug)]
pub struct MediaStore {
    index: SqlitePool,
    directory: PathBuf,
    endpoint: Endpoint,
    downloads: Arc<Mutex<HashMap<Hash, Weak<Mutex<()>>>>>,
    snapshots: Arc<Mutex<HashMap<String, Arc<Snapshot>>>>,
}

fn file_uri(path: &Path) -> Result<String> {
    url::Url::from_file_path(path)
        .map(|uri| uri.to_string())
        .map_err(|()| anyhow::anyhow!("Media path must be absolute"))
}

impl MediaStore {
    pub(crate) async fn open(path: &Path, endpoint: Endpoint) -> Result<Self> {
        tokio::fs::create_dir_all(path).await?;
        let index = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(path.join("files.sqlite"))
                .create_if_missing(true),
        )
        .await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, hash TEXT NOT NULL, size INTEGER NOT NULL, modified TEXT NOT NULL, outboard BLOB NOT NULL)").execute(&index).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS files_hash ON files(hash)")
            .execute(&index)
            .await?;
        Ok(Self {
            index,
            directory: path.to_owned(),
            endpoint,
            downloads: Arc::default(),
            snapshots: Arc::default(),
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.index.close().await;
        self.snapshots.lock().await.clear();
        Ok(())
    }

    /// Cache the hash and range proofs, never a second copy of the audio.
    pub async fn publish(&self, path: &Path) -> Result<String> {
        let path = std::path::absolute(path)?;
        let uri = file_uri(&path)?;
        let metadata = tokio::fs::metadata(&path).await?;
        let size = i64::try_from(metadata.len())?;
        let modified = format!("{:?}", metadata.modified()?.duration_since(UNIX_EPOCH));
        let cached: Option<String> =
            sqlx::query_scalar("SELECT hash FROM files WHERE path=?1 AND size=?2 AND modified=?3")
                .bind(&uri)
                .bind(size)
                .bind(&modified)
                .fetch_optional(&self.index)
                .await?;
        let hash = match cached {
            Some(hash) => hash,
            None => {
                let outboard = tokio::task::spawn_blocking(move || {
                    PreOrderOutboard::<Vec<u8>>::create(std::fs::File::open(path)?, IROH_BLOCK_SIZE)
                })
                .await??;
                let hash = outboard.root.to_hex().to_string();
                sqlx::query("INSERT INTO files VALUES(?1,?2,?3,?4,?5) ON CONFLICT(path) DO UPDATE SET hash=excluded.hash,size=excluded.size,modified=excluded.modified,outboard=excluded.outboard")
                    .bind(&uri).bind(&hash).bind(size).bind(modified).bind(outboard.data).execute(&self.index).await?;
                hash
            }
        };
        Ok(hash)
    }

    /// Keep the offered snapshot alive until this peer requests its next one.
    pub async fn publish_snapshot(
        &self,
        snapshot: tempfile::NamedTempFile,
        peer: &str,
    ) -> Result<String> {
        let file = snapshot.reopen()?;
        let outboard = tokio::task::spawn_blocking(move || {
            PreOrderOutboard::<Vec<u8>>::create(file, IROH_BLOCK_SIZE)
        })
        .await??;
        let hash = outboard.root.to_hex().to_string();
        self.snapshots.lock().await.insert(
            peer.to_owned(),
            Arc::new(Snapshot {
                file: snapshot.into_temp_path(),
                outboard: PreOrderMemOutboard {
                    root: outboard.root,
                    tree: outboard.tree,
                    data: outboard.data.into(),
                },
            }),
        );
        Ok(hash)
    }

    pub async fn forget(&self, hash: &str, keep_file: bool) -> Result<()> {
        if !keep_file {
            sqlx::query("DELETE FROM files WHERE hash=?1")
                .bind(hash)
                .execute(&self.index)
                .await?;
        }
        Ok(())
    }

    /// Folder changes copy the same representation before removing its old path.
    pub async fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let source = tokio::fs::metadata(from).await?;
        let source_modified = format!("{:?}", source.modified()?.duration_since(UNIX_EPOCH));
        let from = file_uri(from)?;
        let to_uri = file_uri(to)?;
        let metadata = tokio::fs::metadata(to).await?;
        let modified = format!("{:?}", metadata.modified()?.duration_since(UNIX_EPOCH));
        let mut tx = self.index.begin().await?;
        let inserted = sqlx::query("INSERT INTO files SELECT ?1,hash,size,?2,outboard FROM files WHERE path=?3 AND size=?4 AND modified=?5 ON CONFLICT(path) DO UPDATE SET hash=excluded.hash,size=excluded.size,modified=excluded.modified,outboard=excluded.outboard")
            .bind(&to_uri).bind(modified).bind(&from).bind(i64::try_from(source.len())?).bind(source_modified).execute(&mut *tx).await?;
        if inserted.rows_affected() != 0 {
            sqlx::query("DELETE FROM files WHERE path=?1")
                .bind(from)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Interrupted downloads keep only their verified prefix. Completed staging
    /// is removed after the caller's destination has been installed.
    pub async fn fetch(
        &self,
        peer: &str,
        hash: &str,
        destination: &Path,
        cancel: CancellationToken,
        progress: watch::Sender<u64>,
    ) -> Result<()> {
        let hash: Hash = hash.parse()?;
        let destination = std::path::absolute(destination)?;
        let lock = {
            let mut downloads = self.downloads.lock().await;
            downloads.retain(|_, lock| lock.strong_count() != 0);
            let lock = downloads
                .get(&hash)
                .and_then(Weak::upgrade)
                .unwrap_or_default();
            downloads.insert(hash, Arc::downgrade(&lock));
            lock
        };
        let _download = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Transfer cancelled"),
            lock = lock.lock() => lock,
        };
        self.download(peer.parse()?, hash, &destination, cancel, progress)
            .await
    }

    async fn download(
        &self,
        peer: EndpointId,
        hash: Hash,
        destination: &Path,
        cancel: CancellationToken,
        progress: watch::Sender<u64>,
    ) -> Result<()> {
        let partial = self.directory.join(format!("{hash}.partial"));
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&partial)
            .await?;
        // A cancelled write may end within a verified 16 KiB leaf. Request that
        // leaf again, including the final leaf of an interrupted completion.
        let length = file.metadata().await?.len();
        let offset = length.saturating_sub(1) / (16 * 1024) * (16 * 1024);
        file.set_len(offset).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        progress.send_replace(offset);
        let request = GetRequest::blob_ranges(hash, ChunkRanges::from(ChunkNum(offset / 1024)..));
        let (mut content, size) = tokio::select! {
            _ = cancel.cancelled() => bail!("Transfer cancelled"),
            result = async {
                let connection = self.endpoint.connect(peer, iroh_blobs::ALPN).await?;
                let connected = fsm::start(connection, request, Default::default()).next().await?;
                let fsm::ConnectedNext::StartRoot(start) = connected.next().await? else { bail!("Missing media response") };
                Ok::<_, anyhow::Error>(start.next().next().await?)
            } => result?,
        };
        let mut file = tokio::io::BufWriter::with_capacity(256 * 1024, file);
        let received: Result<()> = async {
            let mut written = offset;
            let end = loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => bail!("Transfer cancelled"),
                    next = content.next() => next,
                };
                match next {
                    fsm::BlobContentNext::More((next, item)) => {
                        if let BaoContentItem::Leaf(leaf) = item? {
                            if leaf.offset != written {
                                bail!("Non-contiguous media response")
                            }
                            file.write_all(&leaf.data).await?;
                            written += leaf.data.len() as u64;
                            progress.send_replace(written);
                        }
                        content = next;
                    }
                    fsm::BlobContentNext::Done(end) => break end,
                }
            };
            let fsm::EndBlobNext::Closing(closing) = end.next() else {
                bail!("Unexpected media response")
            };
            tokio::select! {
                _ = cancel.cancelled() => bail!("Transfer cancelled"),
                result = closing.next() => { result?; }
            }
            if written != size {
                bail!("Incomplete media response")
            }
            Ok(())
        }
        .await;
        // Finish buffered disk writes before releasing the per-file download
        // lock, including after cancellation or a disconnected peer.
        file.flush().await?;
        received?;
        file.get_ref().sync_all().await?;
        drop(file);
        let parent = destination
            .parent()
            .context("Media destination has no parent")?;
        tokio::fs::create_dir_all(parent).await?;
        match tokio::fs::rename(&partial, destination).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                let staged = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
                tokio::fs::copy(&partial, &staged).await?;
                staged.persist(destination)?;
                tokio::fs::remove_file(partial).await?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn send(&self, hash: Hash, ranges: &ChunkRanges, writer: &mut SendStream) -> Result<()> {
        let snapshot = self
            .snapshots
            .lock()
            .await
            .values()
            .find(|snapshot| snapshot.outboard.root == blake3::Hash::from(hash))
            .cloned();
        if let Some(snapshot) = snapshot {
            let file = iroh_io::File::open(snapshot.file.to_path_buf()).await?;
            writer
                .write_all(&snapshot.outboard.tree.size().to_le_bytes())
                .await?;
            bao_tree::io::fsm::encode_ranges_validated(
                file,
                snapshot.outboard.clone(),
                ranges,
                iroh_io::TokioStreamWriter(writer),
            )
            .await?;
            return Ok(());
        }
        let mut rows = sqlx::query("SELECT path,size,outboard FROM files WHERE hash=?1")
            .bind(hash.to_string())
            .fetch(&self.index);
        use futures_util::TryStreamExt;
        while let Some(row) = rows.try_next().await? {
            let uri: String = row.get(0);
            let path = url::Url::parse(&uri)?
                .to_file_path()
                .map_err(|()| anyhow::anyhow!("Invalid media path"))?;
            let file = match iroh_io::File::open(path).await {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let size = row.get::<i64, _>(1) as u64;
            let outboard = PreOrderMemOutboard {
                root: hash.into(),
                tree: BaoTree::new(size, IROH_BLOCK_SIZE),
                data: row.get::<Vec<u8>, _>(2),
            };
            drop(rows);
            writer.write_all(&size.to_le_bytes()).await?;
            bao_tree::io::fsm::encode_ranges_validated(
                file,
                outboard,
                ranges,
                iroh_io::TokioStreamWriter(writer),
            )
            .await?;
            return Ok(());
        }
        bail!("Media file is unavailable")
    }
}

#[derive(Debug)]
pub(crate) struct MemberBlobs {
    pub network: Weak<ConnectNetwork>,
    pub media: MediaStore,
}

impl ProtocolHandler for MemberBlobs {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.network.upgrade() else {
            return Ok(());
        };
        let peer = connection.remote_id();
        let mut membership = network.membership_changed.subscribe();
        if network.authorize(peer).await.is_err() {
            connection.close(1u32.into(), b"not a profile member");
            return Ok(());
        }
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                stream = connection.accept_bi() => {
                    let Ok((mut writer, mut reader)) = stream else { break };
                    let network = network.clone();
                    let media = self.media.clone();
                    requests.spawn(async move {
                        let result: Result<()> = async {
                            network.authorize(peer).await?;
                            let (request, _) = Request::read_async(&mut reader).await?;
                            network.authorize(peer).await?;
                            match request {
                                Request::Get(request) => {
                                    // Connect publishes raw files, so each request has one root.
                                    if let Some((0, ranges)) = request.ranges.iter_non_empty_infinite().next() {
                                        media.send(request.hash, ranges, &mut writer).await?;
                                    }
                                }
                                Request::GetMany(request) => {
                                    for (index, ranges) in request.ranges.iter_non_empty_infinite() {
                                        let Some(hash) = request.hashes.get(index as usize) else { break };
                                        media.send(*hash, ranges, &mut writer).await?;
                                    }
                                }
                                _ => bail!("Unsupported media request"),
                            }
                            writer.finish()?;
                            Ok(())
                        }.await;
                        if result.is_err() { let _ = writer.reset(3u32.into()); }
                    });
                }
                _ = requests.join_next(), if !requests.is_empty() => {}
                _ = membership.changed() => {
                    if network.authorize(peer).await.is_err() {
                        connection.close(1u32.into(), b"not a profile member");
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
