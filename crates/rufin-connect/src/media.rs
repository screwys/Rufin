//! Verified, resumable blobs on the same endpoint and membership as Connect.
use crate::network::ConnectNetwork;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use iroh::{
    Endpoint, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_blobs::{
    api::{Store, remote::GetProgressItem},
    provider::events::{AbortReason, EventMask, EventSender, ProviderMessage, RequestMode},
    store::fs::FsStore,
};
use std::{path::Path, sync::Weak};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct MediaStore {
    pub(crate) store: Store,
    endpoint: Endpoint,
}

impl MediaStore {
    pub async fn forget(&self, hash: &str, representation: &str) -> Result<()> {
        self.store.tags().delete(format!("download/{hash}")).await?;
        self.store
            .tags()
            .delete(format!("media/{representation}"))
            .await?;
        Ok(())
    }
    pub(crate) async fn open(path: &Path, endpoint: Endpoint) -> Result<Self> {
        let fs = FsStore::load(path).await?;
        Ok(Self {
            store: (*fs).clone(),
            endpoint,
        })
    }

    /// Hash only media selected for serving; corresponding-folder reuse does not
    /// hash the user's existing files. Tags retain originals/derived revisions.
    pub async fn publish(&self, path: &Path, tag: &str) -> Result<String> {
        let absolute = std::path::absolute(path)?;
        let entry = self
            .store
            .blobs()
            .add_path(absolute)
            .with_named_tag(tag)
            .await?;
        Ok(entry.hash.to_string())
    }

    /// Partial ranges remain in the blob store when cancelled or disconnected.
    /// Only a complete, verified blob is exported and renamed into place.
    pub async fn fetch(
        &self,
        peer: &str,
        hash: &str,
        destination: &Path,
        cancel: CancellationToken,
        progress: watch::Sender<u64>,
    ) -> Result<()> {
        let hash = hash.parse::<iroh_blobs::Hash>()?;
        let peer = peer.parse::<EndpointId>()?;
        let connection = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Transfer cancelled"),
            connection = self.endpoint.connect(peer, iroh_blobs::ALPN) => connection?,
        };
        let stream = self.store.remote().fetch(connection, hash).stream();
        tokio::pin!(stream);
        loop {
            let next = tokio::select! { _ = cancel.cancelled() => bail!("Transfer cancelled"), next = stream.next() => next };
            match next.context("Media transfer ended before completion")? {
                GetProgressItem::Progress(bytes) => {
                    progress.send_replace(bytes);
                }
                GetProgressItem::Error(error) => return Err(error.into()),
                GetProgressItem::Done(_) => break,
            }
        }
        let destination = std::path::absolute(destination)?;
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let partial = tempfile::NamedTempFile::new_in(
            destination
                .parent()
                .context("Media destination has no parent")?,
        )?
        .into_temp_path();
        self.store.blobs().export(hash, &partial).await?;
        if cancel.is_cancelled() {
            bail!("Transfer cancelled");
        }
        partial.persist(destination)?;
        // Keep downloaded content available for serving and restarting exports.
        self.store
            .tags()
            .set(format!("download/{hash}"), hash)
            .await?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct MemberBlobs {
    pub network: Weak<ConnectNetwork>,
    pub store: Store,
}

impl ProtocolHandler for MemberBlobs {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.network.upgrade() else {
            return Ok(());
        };
        let peer = connection.remote_id();
        if network.authorize(peer).await.is_err() {
            connection.close(1u32.into(), b"not a profile member");
            return Ok(());
        }
        // Intercept each request, including later requests on this connection.
        // Throttle interception also stops an active transfer after removal.
        let (events, mut requests) = EventSender::channel(
            16,
            EventMask {
                get: RequestMode::Intercept,
                get_many: RequestMode::Intercept,
                ..EventMask::ALL_READONLY
            },
        );
        let authorize = async {
            while let Some(request) = requests.recv().await {
                let result = network
                    .authorize(peer)
                    .await
                    .map_err(|_| AbortReason::Permission);
                match request {
                    ProviderMessage::ClientConnected(msg) => {
                        let _ = msg.tx.send(result).await;
                    }
                    ProviderMessage::GetRequestReceived(msg) => {
                        let _ = msg.tx.send(result).await;
                    }
                    ProviderMessage::GetManyRequestReceived(msg) => {
                        let _ = msg.tx.send(result).await;
                    }
                    ProviderMessage::ObserveRequestReceived(msg) => {
                        let _ = msg.tx.send(result).await;
                    }
                    ProviderMessage::Throttle(msg) => {
                        let _ = msg.tx.send(result).await;
                    }
                    _ => {}
                }
            }
        };
        let provide =
            iroh_blobs::provider::handle_connection(connection, self.store.clone(), events);
        tokio::select! { _ = authorize => {}, _ = provide => {} }
        Ok(())
    }
}
