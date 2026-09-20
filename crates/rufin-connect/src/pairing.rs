//! Vodozemac's SAS binds the two Iroh TLS device identities. Both displays must
//! be approved before trusted-device enrollment is exchanged.
use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use iroh::{
    endpoint::{Connection, ConnectionError},
    protocol::{AcceptError, ProtocolHandler},
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use vodozemac::sas::{Mac, Sas};

use crate::network::{ConnectNetwork, NetworkEvent, read_frame, write_frame};

// Matrix's specified 64-symbol emoji alphabet. The seven indices are produced
// by vodozemac; no Matrix account or Matrix transport is involved.
const EMOJI: [&str; 64] = [
    "🐶", "🐱", "🦁", "🐎", "🦄", "🐷", "🐘", "🐰", "🐼", "🐓", "🐧", "🐢", "🐟", "🐙", "🦋", "🌷",
    "🌳", "🌵", "🍄", "🌏", "🌙", "☁️", "🔥", "🍌", "🍎", "🍓", "🌽", "🍕", "🎂", "❤️", "😀", "🤖",
    "🎩", "👓", "🔧", "🎅", "👍", "☂️", "⌛", "⏰", "🎁", "💡", "📕", "✏️", "📎", "✂️", "🔒", "🔑",
    "🔨", "☎️", "🏁", "🚂", "🚲", "✈️", "🚀", "🏆", "⚽", "🎸", "🎺", "🔔", "⚓", "🎧", "📁", "📌",
];

#[derive(Serialize, Deserialize)]
struct Hello {
    name: String,
    identity: String,
    commitment: String,
}

#[derive(Serialize, Deserialize)]
struct Enrollment {
    profile_id: String,
    data: Vec<u8>,
    roster: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct PairProtocol(pub Weak<ConnectNetwork>);
impl ProtocolHandler for PairProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Some(network) = self.0.upgrade() {
            let _ = run(network, conn, false).await;
        }
        Ok(())
    }
}

pub(crate) async fn run(
    network: Arc<ConnectNetwork>,
    connection: Connection,
    joining: bool,
) -> Result<()> {
    let mut session = None;
    let result = tokio::time::timeout(Duration::from_secs(300), async {
        tokio::select! {
            biased;
            result = exchange(network.clone(), &connection, joining, &mut session) => result,
            error = connection.closed() => Err(error).context("The other device ended pairing"),
        }
    })
    .await
    .context("Pairing timed out")
    .and_then(|result| result);
    network
        .approvals
        .lock()
        .await
        .retain(|_, approval| !approval.is_closed());
    if let Err(error) = &result {
        connection.close(1u32.into(), b"pairing ended");
        let _ = network
            .events
            .send(NetworkEvent::PairingFailed {
                session,
                error: error.to_string(),
            })
            .await;
    }
    result
}

async fn exchange(
    network: Arc<ConnectNetwork>,
    connection: &Connection,
    joining: bool,
    attempt: &mut Option<String>,
) -> Result<()> {
    if !joining {
        network.current_profile().await?;
    }

    let peer = connection.remote_id();
    ensure!(
        peer != network.endpoint.id(),
        "Cannot pair a device with itself"
    );
    let sas = Sas::new();
    let public = sas.public_key().to_base64();
    let hello = Hello {
        name: network.name.read().await.clone(),
        identity: network.identity(),
        commitment: blake3::hash(public.as_bytes()).to_hex().to_string(),
    };
    let (mut send, mut recv) = if joining {
        connection.open_bi().await?
    } else {
        connection.accept_bi().await?
    };
    // Commit both ephemeral keys before revealing either. This prevents choosing
    // a key after seeing the other's key to bias the short comparison.
    write_frame(&mut send, &hello).await?;
    let remote: Hello = read_frame(&mut recv).await?;
    ensure!(
        remote.identity == peer.to_string(),
        "Enrollment identity differs from the connected device"
    );
    write_frame(&mut send, &public).await?;
    let remote_public: String = read_frame(&mut recv).await?;
    ensure!(
        blake3::hash(remote_public.as_bytes()).to_hex().as_str() == remote.commitment,
        "Verification key commitment does not match"
    );
    let sas = sas.diffie_hellman_with_raw(&remote_public)?;
    let (joiner, host, joiner_key, host_key) = if joining {
        (&hello, &remote, &public, &remote_public)
    } else {
        (&remote, &hello, &remote_public, &public)
    };
    let transcript =
        serde_json::to_string(&("Rufin Connect SAS 1", joiner, host, joiner_key, host_key))?;
    let session = blake3::hash(transcript.as_bytes()).to_hex().to_string();
    *attempt = Some(session.clone());
    let emoji = sas
        .bytes(&transcript)
        .emoji_indices()
        .map(|index| EMOJI[index as usize].to_string());
    let (approve, mut decision) = watch::channel(None);
    network
        .approvals
        .lock()
        .await
        .insert(session.clone(), approve);
    network
        .events
        .send(NetworkEvent::Pairing {
            session: session.clone(),
            peer: peer.to_string(),
            name: remote.name.clone(),
            emoji,
        })
        .await
        .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
    let mac = sas
        .calculate_mac(&network.identity(), &transcript)
        .to_base64();
    let remote_mac = exchange_approval(&mut send, &mut recv, &mut decision, mac).await?;
    let remote_mac = Mac::from_base64(&remote_mac)?;
    sas.verify_mac(&peer.to_string(), &transcript, &remote_mac)?;
    let enrollment = async {
        network
            .events
            .send(NetworkEvent::PairingVerified { session })
            .await
            .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
        if joining {
            let enrollment: Enrollment = read_frame(&mut recv).await?;
            network
                .accept_enrollment(&enrollment.profile_id, &enrollment.roster, peer)
                .await?;
            // Core applies the verified profile data before opening the profile.
            // Pairing never mutates Rufin's existing library.
            network
                .events
                .send(NetworkEvent::Paired {
                    profile_id: enrollment.profile_id,
                    peer: peer.to_string(),
                    name: remote.name,
                    enrollment_data: enrollment.data,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            write_frame(&mut send, &true).await?;
            send.finish()?;
            let complete: bool = read_frame(&mut recv).await?;
            ensure!(complete, "The other device did not complete pairing");
            // Iroh requires the last application-data receiver to close. The host
            // keeps its connection alive until this final acknowledgement arrives.
            connection.close(0u32.into(), b"pairing complete");
        } else {
            let profile_id = network.current_profile().await?;
            let roster = network.enroll(peer, &remote.name).await?;
            write_frame(
                &mut send,
                &Enrollment {
                    profile_id,
                    data: network.enrollment_data.read().await.clone(),
                    roster,
                },
            )
            .await?;
            let accepted: bool = read_frame(&mut recv).await?;
            ensure!(accepted, "The other device did not accept enrollment");
            network
                .events
                .send(NetworkEvent::MemberAdded {
                    peer: peer.to_string(),
                    name: remote.name,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            write_frame(&mut send, &true).await?;
            send.finish()?;
            match connection.closed().await {
                ConnectionError::ApplicationClosed(close) if close.error_code == 0u32.into() => {}
                error => return Err(error).context("The other device ended pairing"),
            }
        }
        Ok(())
    };
    tokio::select! {
        biased;
        _ = decision.wait_for(|value| *value == Some(false)) => anyhow::bail!("Pairing was cancelled"),
        result = enrollment => result,
    }
}

// Like Matrix SAS, local confirmation and the peer's MAC are independent.
// Keep reading the same message while local confirmation changes.
async fn exchange_approval(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
    decision: &mut watch::Receiver<Option<bool>>,
    mac: String,
) -> Result<String> {
    // Keep the same read alive when local approval wins. read_frame may already
    // have consumed its length prefix or part of its body at that point.
    let remote_approval = read_frame::<Option<String>>(recv);
    tokio::pin!(remote_approval);
    let (approved, remote_mac) = tokio::select! {
        approved = async { decision.wait_for(|value| value.is_some()).await
            .ok().and_then(|value| *value).unwrap_or(false) } => (approved, None),
        remote = &mut remote_approval => {
            let remote = remote?.context("The other device rejected pairing")?;
            let approved = decision.wait_for(|value| value.is_some()).await
                .ok().and_then(|value| *value).unwrap_or(false);
            (approved, Some(remote))
        }
    };
    // The approval MAC binds TLS identity, role, key exchange, and this unique
    // verification session.
    write_frame(send, &approved.then_some(mac)).await?;
    ensure!(approved, "Pairing was rejected");
    Ok(match remote_mac {
        Some(mac) => mac,
        None => tokio::select! {
            remote = &mut remote_approval => remote?.context("The other device rejected pairing")?,
            _ = decision.wait_for(|value| *value == Some(false)) => anyhow::bail!("Pairing was rejected"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn local_confirmation_preserves_a_partly_received_peer_approval() {
        for split in [1, 4, 7] {
            let (local, remote) = tokio::io::duplex(1024);
            let (mut recv, mut send) = tokio::io::split(local);
            let (mut remote_recv, mut remote_send) = tokio::io::split(remote);
            let (approve, mut decision) = watch::channel(None);
            let body = serde_json::to_vec(&Some("peer approval")).unwrap();
            let mut frame = (body.len() as u32).to_be_bytes().to_vec();
            frame.extend(body);
            remote_send.write_all(&frame[..split]).await.unwrap();
            let exchange =
                exchange_approval(&mut send, &mut recv, &mut decision, "local approval".into());
            tokio::pin!(exchange);
            assert!(futures_util::poll!(&mut exchange).is_pending());
            approve.send(Some(true)).unwrap();
            let peer = async {
                let mac: Option<String> = read_frame(&mut remote_recv).await.unwrap();
                assert_eq!(mac.as_deref(), Some("local approval"));
                remote_send.write_all(&frame[split..]).await.unwrap();
            };
            let (result, _) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(exchange, peer)
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap(), "peer approval");
        }
    }

    #[tokio::test]
    async fn confirmed_pairing_can_be_cancelled_while_waiting_for_peer() {
        let (local, mut remote) = tokio::io::duplex(1024);
        let (mut recv, mut send) = tokio::io::split(local);
        let (approve, mut decision) = watch::channel(Some(true));
        let peer = async {
            let _: Option<String> = read_frame(&mut remote).await.unwrap();
            approve.send(Some(false)).unwrap();
        };
        let (result, _) = tokio::join!(
            exchange_approval(&mut send, &mut recv, &mut decision, "local approval".into()),
            peer,
        );
        assert!(result.unwrap_err().to_string().contains("rejected"));
    }

    #[test]
    fn comparison_and_approval_are_bound_to_device_identities() {
        let a = Sas::new();
        let b = Sas::new();
        let a_key = a.public_key();
        let b_key = b.public_key();
        let a = a.diffie_hellman(b_key).unwrap();
        let b = b.diffie_hellman(a_key).unwrap();
        let transcript = "Rufin Connect SAS 1|joiner:alice device key|host:bob device key";
        assert_eq!(
            a.bytes(transcript).emoji_indices(),
            b.bytes(transcript).emoji_indices()
        );
        let mac = a.calculate_mac("alice device key", transcript);
        assert!(b.verify_mac("alice device key", transcript, &mac).is_ok());
        assert!(
            b.verify_mac("mallory device key", transcript, &mac)
                .is_err()
        );
        assert!(
            b.verify_mac("alice device key", "another enrollment", &mac)
                .is_err()
        );
    }
}
