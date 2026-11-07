//! Record sync. A device that wrote or applied records tells the others its sequence; each pulls
//! the rows after its cursor in one stream. A device serves every row it holds, so rows travel on
//! through any device that is online.
use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, future::BoxFuture};
use iroh::{
    endpoint::{Connection, ConnectionError, RecvStream, SendStream},
    protocol::{AcceptError, ProtocolHandler},
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{
    network::{
        ConnectNetwork, MAX_FRAME, NetworkEvent, Profile, SYNC_PROTOCOL, TRUSTED_FRAME, read_frame,
        write_frame,
    },
    store::Row,
};

const PAGE: usize = 512;
// Rows fill a frame up to this size; a larger row, such as a big playlist cover or a long
// queue, goes in a frame of its own.
const PAGE_BYTES: usize = 8 * 1024 * 1024;
const PUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Encodes the values a pull or a folder segment sends from the Store and settings.
pub trait Values: Send + Sync {
    /// Fills in the value of each live row that holds none, as it leaves this device. A row
    /// whose item is gone keeps none and is not sent; the next diff pass tombstones it.
    fn encode<'a>(&'a self, rows: &'a mut [Row]) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Serialize, Deserialize)]
enum Request {
    /// A joining device holds none of the rows it wrote; any other has them.
    Pull {
        profile: String,
        after: i64,
        joining: bool,
    },
    Changed {
        profile: String,
        sequence: i64,
    },
}

/// What a pull reads after its row count.
#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    Page(Vec<Row>),
    /// The last sequence the device examined; rows it left out lie at or below it.
    End {
        sequence: i64,
    },
}

pub struct Pull {
    /// The rows after the cursor when the pull started.
    pub total: u64,
    connection: Connection,
    recv: RecvStream,
}

impl Pull {
    pub async fn next(&mut self) -> Result<Frame> {
        let frame = read_frame(&mut self.recv, TRUSTED_FRAME).await?;
        if let Frame::End { .. } = frame {
            // The device keeps its connection until this, the last read, is done.
            self.connection.close(0u32.into(), b"pull complete");
        }
        Ok(frame)
    }
}

impl ConnectNetwork {
    /// Pull the rows a trusted device holds after `after`.
    pub async fn pull(&self, peer: &str, profile: &str, after: i64, joining: bool) -> Result<Pull> {
        let connection =
            tokio::time::timeout(Duration::from_secs(30), self.connect(peer, SYNC_PROTOCOL))
                .await
                .context("Device is unreachable")??;
        let opened = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(
                &mut send,
                &Request::Pull {
                    profile: profile.to_owned(),
                    after,
                    joining,
                },
                MAX_FRAME,
            )
            .await?;
            send.finish()?;
            let total: u64 = read_frame(&mut recv, MAX_FRAME).await?;
            Ok((total, recv))
        }
        .await;
        let (total, recv) = self.settle(&connection, opened).await?;
        Ok(Pull {
            total,
            connection,
            recv,
        })
    }

    /// Tell every trusted device that this device's records reached `sequence`. Returns once
    /// each has pulled or its 5 seconds passed.
    pub async fn push(&self, sequence: i64) {
        let Some(profile) = self
            .profile
            .read()
            .await
            .as_ref()
            .map(|profile| profile.id.clone())
        else {
            return;
        };
        let request = Request::Changed { profile, sequence };
        futures_util::stream::iter(self.trusted_devices())
            .for_each_concurrent(None, |peer| {
                let request = &request;
                async move {
                    let _ = tokio::time::timeout(PUSH_TIMEOUT, self.notify(&peer, request)).await;
                }
            })
            .await;
    }

    async fn notify(&self, peer: &str, request: &Request) -> Result<()> {
        let connection = self.connect(peer, SYNC_PROTOCOL).await?;
        let notified = async {
            let (mut send, _recv) = connection.open_bi().await?;
            write_frame(&mut send, request, MAX_FRAME).await?;
            send.finish()?;
            match connection.closed().await {
                ConnectionError::ApplicationClosed(close) if close.error_code == 0u32.into() => {
                    Ok(())
                }
                error => Err(error.into()),
            }
        }
        .await;
        self.settle(&connection, notified).await
    }
}

#[derive(Debug)]
pub(crate) struct SyncProtocol(pub Weak<ConnectNetwork>);
impl ProtocolHandler for SyncProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.0.upgrade() else {
            return Ok(());
        };
        if !network.admit(&connection).await {
            return Ok(());
        }
        let result: Result<()> = async {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let request: Request = read_frame(&mut recv, MAX_FRAME).await?;
            let profile = network
                .profile
                .read()
                .await
                .clone()
                .context("Rufin Connect is disabled")?;
            match request {
                Request::Changed {
                    profile: id,
                    sequence,
                } => {
                    ensure!(id == profile.id, "The Connect profile changed");
                    let (pulled, done) = oneshot::channel();
                    network
                        .events
                        .send(NetworkEvent::Changed {
                            peer: connection.remote_id().to_string(),
                            sequence,
                            pulled,
                        })
                        .await
                        .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
                    tokio::select! {
                        _ = done => connection.close(0u32.into(), b"changed"),
                        _ = connection.closed() => {}
                    }
                }
                Request::Pull {
                    profile: id,
                    after,
                    joining,
                } => {
                    ensure!(id == profile.id, "The Connect profile changed");
                    let held = (!joining).then(|| connection.remote_id().to_string());
                    serve(&profile, held, after, &mut send).await?;
                    send.finish()?;
                    let _ = connection.closed().await;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::debug!(%error, "Connect sync ended");
            connection.close(1u32.into(), b"sync ended");
        }
        Ok(())
    }
}

/// Sends the rows after `after`, but none `held` wrote.
async fn serve(
    profile: &Arc<Profile>,
    held: Option<String>,
    mut after: i64,
    send: &mut SendStream,
) -> Result<()> {
    write_frame(send, &profile.store.count_after(after).await?, MAX_FRAME).await?;
    loop {
        let mut rows = profile.store.rows_after(after, PAGE, None).await?;
        let Some(last) = rows.last() else {
            break;
        };
        after = last.sequence;
        let complete = rows.len() < PAGE;
        rows.retain(|row| Some(&row.device) != held.as_ref());
        profile
            .values
            .encode(&mut rows)
            .await
            .map_err(anyhow::Error::msg)?;
        rows.retain(|row| row.deleted || row.value.is_some());
        write_page(send, rows).await?;
        if complete {
            break;
        }
    }
    write_frame(send, &Frame::End { sequence: after }, MAX_FRAME).await
}

async fn write_page(send: &mut SendStream, rows: Vec<Row>) -> Result<()> {
    let mut page = Vec::new();
    let mut size = 0;
    for row in rows {
        let length = serde_json::to_vec(&row)?.len() + 1;
        if size + length > PAGE_BYTES && !page.is_empty() {
            write_frame(send, &Frame::Page(std::mem::take(&mut page)), TRUSTED_FRAME).await?;
            size = 0;
        }
        size += length;
        page.push(row);
    }
    if !page.is_empty() {
        write_frame(send, &Frame::Page(page), TRUSTED_FRAME).await?;
    }
    Ok(())
}
