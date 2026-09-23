use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use presence::Activity;
use serde_json::{Value, json};
use tracing::debug;

use super::{LatestReceiver, LatestSender, latest_slot};

#[cfg(unix)]
mod transport {
    #[cfg(not(test))]
    use std::env;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    pub(super) type IpcStream = UnixStream;

    pub(super) fn connect(paths: &[PathBuf]) -> Result<IpcStream, String> {
        for path in paths {
            if let Ok(stream) = UnixStream::connect(path) {
                return Ok(stream);
            }
        }
        Err("Discord IPC socket was not found".to_string())
    }

    pub(super) fn configure(stream: &IpcStream) -> Result<(), String> {
        stream
            .set_read_timeout(Some(Duration::from_millis(750)))
            .and_then(|()| stream.set_write_timeout(Some(Duration::from_millis(750))))
            .map_err(|error| error.to_string())
    }

    #[cfg(not(test))]
    pub(super) fn paths() -> Vec<PathBuf> {
        let xdg_runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let temporary_roots = ["TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .filter_map(|key| env::var_os(key).map(PathBuf::from))
            .collect();
        paths_for(xdg_runtime_dir, temporary_roots)
    }

    fn paths_for(xdg_runtime_dir: Option<PathBuf>, temporary_roots: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(path) = xdg_runtime_dir {
            roots.push(path.clone());
            roots.push(path.join("app/com.discordapp.Discord"));
        }
        for path in temporary_roots {
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
        let tmp = PathBuf::from("/tmp");
        if !roots.contains(&tmp) {
            roots.push(tmp);
        }
        roots
            .into_iter()
            .flat_map(|root| (0..10).map(move |index| root.join(format!("discord-ipc-{index}"))))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use std::path::PathBuf;

        use super::paths_for;

        #[test]
        fn paths_include_native_and_flatpak_discord() {
            let paths = paths_for(
                Some(PathBuf::from("/run/user/1000")),
                vec![PathBuf::from("/tmp/discord")],
            );

            assert!(paths.contains(&PathBuf::from("/run/user/1000/discord-ipc-0")));
            assert!(paths.contains(&PathBuf::from(
                "/run/user/1000/app/com.discordapp.Discord/discord-ipc-0"
            )));
            assert!(paths.contains(&PathBuf::from(
                "/run/user/1000/app/com.discordapp.Discord/discord-ipc-9"
            )));
        }

        #[test]
        fn paths_include_the_native_macos_temporary_directory() {
            let root = PathBuf::from("/var/folders/ab/example/T");
            let paths = paths_for(None, vec![root.clone()]);

            assert!(paths.contains(&root.join("discord-ipc-0")));
            assert!(paths.contains(&root.join("discord-ipc-9")));
        }
    }
}

#[cfg(windows)]
mod transport {
    use std::fs::{File, OpenOptions};
    use std::path::PathBuf;

    pub(super) type IpcStream = File;

    pub(super) fn connect(paths: &[PathBuf]) -> Result<IpcStream, String> {
        for path in paths {
            if let Ok(stream) = OpenOptions::new().read(true).write(true).open(path) {
                return Ok(stream);
            }
        }
        Err("Discord IPC named pipe was not found".to_string())
    }

    pub(super) fn configure(_stream: &IpcStream) -> Result<(), String> {
        Ok(())
    }

    pub(super) fn paths() -> Vec<PathBuf> {
        (0..10)
            .map(|index| PathBuf::from(format!(r"\\?\pipe\discord-ipc-{index}")))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use std::path::PathBuf;

        use super::paths;

        #[test]
        fn paths_cover_every_discord_named_pipe() {
            let paths = paths();

            assert_eq!(paths.len(), 10);
            assert_eq!(
                paths.first(),
                Some(&PathBuf::from(r"\\?\pipe\discord-ipc-0"))
            );
            assert_eq!(
                paths.last(),
                Some(&PathBuf::from(r"\\?\pipe\discord-ipc-9"))
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod transport {
    use std::io::Cursor;
    #[cfg(not(test))]
    use std::path::PathBuf;

    pub(super) type IpcStream = Cursor<Vec<u8>>;

    pub(super) fn connect(_paths: &[PathBuf]) -> Result<IpcStream, String> {
        Err("Discord rich presence is unavailable on this platform".to_string())
    }

    pub(super) fn configure(_stream: &IpcStream) -> Result<(), String> {
        Ok(())
    }

    #[cfg(not(test))]
    pub(super) fn paths() -> Vec<PathBuf> {
        Vec::new()
    }
}

use transport::IpcStream;

pub(crate) const SUPPORTED: bool = cfg!(any(unix, windows));

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

const IPC_VERSION: u8 = 1;
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

pub(crate) struct Worker {
    mailbox: Option<LatestSender<Option<Arc<Activity>>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn new() -> Self {
        let (mailbox, receiver) = latest_slot();
        let thread = std::thread::spawn(move || {
            run_worker(&receiver, Connection::new(), RECONNECT_DELAY);
        });
        Self {
            mailbox: Some(mailbox),
            thread: Some(thread),
        }
    }

    pub(crate) fn publish(&self, activity: Option<Arc<Activity>>) {
        if let Some(mailbox) = &self.mailbox {
            mailbox.publish(activity);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mailbox) = self.mailbox.take() {
            mailbox.publish(None);
            drop(mailbox);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_worker(
    receiver: &LatestReceiver<Option<Arc<Activity>>>,
    mut connection: Connection,
    reconnect_delay: Duration,
) {
    let mut current = receiver.recv();
    while let Some(activity) = current {
        if connection.apply(activity.as_deref()) {
            current = match receiver.recv_timeout(reconnect_delay) {
                Ok(next) => Some(next),
                Err(RecvTimeoutError::Timeout) => Some(activity),
                Err(RecvTimeoutError::Disconnected) => None,
            };
        } else {
            drop(activity);
            current = receiver.recv();
        }
    }
}

struct Connection {
    stream: Option<IpcStream>,
    client_id: Option<String>,
    paths: Vec<PathBuf>,
    nonce: u64,
}

impl Connection {
    fn new() -> Self {
        Self {
            stream: None,
            client_id: None,
            paths: worker_ipc_paths(),
            nonce: 0,
        }
    }

    fn apply(&mut self, activity: Option<&Activity>) -> bool {
        if !SUPPORTED {
            let _ = activity;
            debug!("Discord rich presence is not supported on this platform");
            return false;
        }
        let Some(activity) = activity else {
            if self.stream.is_some() {
                let payload = self.activity_payload(None);
                if self.send_payload(&payload).is_err() {
                    self.disconnect();
                }
            }
            return false;
        };
        let client_id = activity.client_id();
        if self
            .client_id
            .as_deref()
            .is_some_and(|old| old != client_id)
        {
            let payload = self.activity_payload(None);
            let _ = self.send_payload(&payload);
            self.disconnect();
        }
        if let Err(error) = self.ensure_connected(client_id) {
            debug!(%error, "Discord IPC connection unavailable");
            return true;
        }
        let payload = self.activity_payload(Some(activity));
        if let Err(error) = self.send_payload(&payload) {
            debug!(%error, "Discord IPC update failed");
            self.disconnect();
            return true;
        }
        false
    }

    fn activity_payload(&mut self, activity: Option<&Activity>) -> Value {
        self.nonce = self.nonce.wrapping_add(1);
        json!({
            "cmd": "SET_ACTIVITY",
            "args": {
                "pid": std::process::id(),
                "activity": activity.map(Activity::json),
            },
            "nonce": format!("rufin-{}", self.nonce),
        })
    }

    fn ensure_connected(&mut self, client_id: &str) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }
        let mut stream = transport::connect(&self.paths)?;
        transport::configure(&stream)?;
        write_packet(
            &mut stream,
            OP_HANDSHAKE,
            &json!({ "v": IPC_VERSION, "client_id": client_id }),
        )?;
        read_response(&mut stream)?;
        self.stream = Some(stream);
        self.client_id = Some(client_id.to_string());
        Ok(())
    }

    fn send_payload(&mut self, payload: &Value) -> Result<(), String> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| "Discord IPC is not connected".to_string())?;
        write_packet(stream, OP_FRAME, payload)?;
        read_response(stream)
    }

    fn disconnect(&mut self) {
        self.stream = None;
        self.client_id = None;
    }
}

fn worker_ipc_paths() -> Vec<PathBuf> {
    #[cfg(test)]
    {
        Vec::new()
    }
    #[cfg(not(test))]
    {
        transport::paths()
    }
}

fn write_packet(stream: &mut IpcStream, opcode: u32, payload: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
    let length = u32::try_from(bytes.len()).map_err(|_| "Discord IPC payload is too large")?;
    stream
        .write_all(&opcode.to_le_bytes())
        .and_then(|()| stream.write_all(&length.to_le_bytes()))
        .and_then(|()| stream.write_all(&bytes))
        .map_err(|error| error.to_string())
}

fn read_packet(stream: &mut IpcStream) -> Result<(u32, Value), String> {
    let mut header = [0_u8; 8];
    stream
        .read_exact(&mut header)
        .map_err(|error| error.to_string())?;
    let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut bytes = vec![0_u8; length];
    stream
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    let value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if opcode == OP_PING {
        write_packet(stream, OP_PONG, &value)?;
    }
    Ok((opcode, value))
}

fn read_response(stream: &mut IpcStream) -> Result<(), String> {
    loop {
        match read_packet(stream)? {
            (OP_PING, _) => {}
            (OP_CLOSE, response) => return Err(format!("Discord IPC closed: {response}")),
            (OP_FRAME, _) => return Ok(()),
            (opcode, _) => return Err(format!("unexpected Discord IPC opcode {opcode}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playback::TransportStatus;
    use presence::{APP_ICON_ASSET, LinkType, Settings};

    #[test]
    fn lastfm_album_url_uses_the_album_display_artist() {
        let view = super::super::tests::test_view(1, "Album", TransportStatus::Playing, 0);
        let activity = Activity::new(
            &Settings {
                enabled: true,
                link_type: LinkType::LastFm,
                ..Settings::default()
            },
            &view,
            0,
            APP_ICON_ASSET.to_string(),
        )
        .expect("visible activity");

        let payload = activity.json();
        let details = payload["details_url"].as_str();
        let state = payload["state_url"].as_str();
        assert_eq!(
            details,
            Some("https://www.last.fm/music/Album%20Artist/Album/Track")
        );
        assert_eq!(state, Some("https://www.last.fm/music/Artist"));
    }

    #[test]
    fn musicbrainz_links_use_the_queue_snapshot_ids() {
        let view = super::super::tests::test_view(1, "Album", TransportStatus::Playing, 0);
        let activity = Activity::new(
            &Settings {
                enabled: true,
                link_type: LinkType::MusicBrainz,
                ..Settings::default()
            },
            &view,
            0,
            APP_ICON_ASSET.to_string(),
        )
        .expect("visible activity");

        let payload = activity.json();
        let details = payload["details_url"].as_str();
        let state = payload["state_url"].as_str();
        assert_eq!(details, Some("https://musicbrainz.org/track/track-id"));
        assert_eq!(state, Some("https://musicbrainz.org/artist/artist-id"));
    }

    #[cfg(unix)]
    #[test]
    fn handshake_answers_ping_before_accepting_the_frame() {
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair().expect("IPC pair");
        let peer = std::thread::spawn(move || {
            let (opcode, payload) = read_packet(&mut server).expect("read handshake");
            assert_eq!(opcode, OP_HANDSHAKE);
            assert_eq!(payload["v"], IPC_VERSION);
            assert_eq!(payload["client_id"], "client");
            write_packet(&mut server, OP_PING, &json!({"nonce": 7})).expect("write ping");
            write_packet(&mut server, OP_FRAME, &json!({"evt": "READY"}))
                .expect("write ready frame");
            let (opcode, payload) = read_packet(&mut server).expect("read pong");
            assert_eq!(opcode, OP_PONG);
            assert_eq!(payload["nonce"], 7);
        });

        write_packet(
            &mut client,
            OP_HANDSHAKE,
            &json!({"v": IPC_VERSION, "client_id": "client"}),
        )
        .expect("write handshake");
        read_response(&mut client).expect("accept ready response");
        peer.join().expect("IPC peer");
    }

    #[cfg(unix)]
    #[test]
    fn failed_update_disconnects_so_the_worker_can_reconnect() {
        let (client, peer) = std::os::unix::net::UnixStream::pair().expect("IPC pair");
        drop(peer);
        let mut connection = Connection {
            stream: Some(client),
            client_id: Some("client".to_string()),
            paths: Vec::new(),
            nonce: 0,
        };
        let view = super::super::tests::test_view(1, "Album", TransportStatus::Playing, 0);
        let activity = Activity::new(
            &Settings {
                enabled: true,
                client_id: "client".to_string(),
                ..Settings::default()
            },
            &view,
            0,
            APP_ICON_ASSET.to_string(),
        )
        .expect("visible activity");

        assert!(connection.apply(Some(&activity)));
        assert!(connection.stream.is_none());
        assert!(connection.client_id.is_none());
    }
}
