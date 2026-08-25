use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use playback::{CurrentMedia, PlaybackView, TransportStatus};
use serde::{Deserialize, Serialize};
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

pub const DEFAULT_CLIENT_ID: &str = "1505345384686419979";
pub(crate) const APP_ICON_URL: &str = "https://raw.githubusercontent.com/screwys/Rufin/main/data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg";
pub(crate) const SUPPORTED: bool = cfg!(any(unix, windows));

const MAX_TEXT_LENGTH: usize = 127;
const MAX_URL_LENGTH: usize = 256;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

const IPC_VERSION: u8 = 1;
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DisplayType {
    #[serde(rename = "artist")]
    Artist,
    #[serde(rename = "application", alias = "app")]
    #[default]
    Application,
    #[serde(rename = "song")]
    Song,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LinkType {
    #[serde(rename = "last_fm")]
    LastFm,
    #[serde(rename = "musicbrainz")]
    #[default]
    MusicBrainz,
    #[serde(rename = "musicbrainz_last_fm")]
    MusicBrainzLastFm,
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    #[serde(rename = "discord_presence_enabled")]
    pub enabled: bool,
    #[serde(rename = "discord_client_id")]
    pub client_id: String,
    #[serde(rename = "discord_display_type")]
    pub display_type: DisplayType,
    #[serde(rename = "discord_link_type")]
    pub link_type: LinkType,
    #[serde(rename = "discord_show_paused")]
    pub show_paused: bool,
    #[serde(rename = "discord_show_as_listening")]
    pub show_as_listening: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: DEFAULT_CLIENT_ID.to_string(),
            display_type: DisplayType::Application,
            link_type: LinkType::MusicBrainz,
            show_paused: false,
            show_as_listening: true,
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.client_id = self.client_id.trim().to_string();
        if self.client_id.is_empty() {
            self.client_id = DEFAULT_CLIENT_ID.to_string();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlaybackState {
    Playing,
    Paused,
}

#[derive(Clone)]
pub(crate) struct Activity {
    settings: Settings,
    media: Arc<CurrentMedia>,
    playback_state: PlaybackState,
    pub(crate) started_at_millis: Option<u64>,
    pub(crate) ended_at_millis: Option<u64>,
    pub(crate) large_image: String,
}

impl Activity {
    pub(crate) fn new(
        settings: &Settings,
        view: &PlaybackView,
        now_millis: u64,
        large_image: String,
    ) -> Option<Self> {
        let playback_state = visible_playback_state(settings, view.transport.state)?;
        let media = Arc::clone(view.transport.current.as_ref()?);
        media.id.run?;
        let duration_millis = duration_millis(&media);
        let started_at_millis = match playback_state {
            PlaybackState::Playing => {
                Some(now_millis.saturating_sub(view.transport.position_millis))
            }
            PlaybackState::Paused => None,
        };
        Some(Self {
            settings: settings.clone(),
            media,
            playback_state,
            started_at_millis,
            ended_at_millis: started_at_millis.and_then(|started| {
                (duration_millis > 0).then(|| started.saturating_add(duration_millis))
            }),
            large_image,
        })
    }

    pub(crate) fn matches(&self, view: &PlaybackView) -> bool {
        Some(self.playback_state) == visible_playback_state(&self.settings, view.transport.state)
            && view
                .transport
                .current
                .as_ref()
                .is_some_and(|media| media.as_ref() == self.media.as_ref())
    }
}

fn duration_millis(media: &CurrentMedia) -> u64 {
    u64::try_from(media.track.duration_millis.max(0)).unwrap_or(u64::MAX)
}

pub(crate) fn visible_playback_state(
    settings: &Settings,
    state: TransportStatus,
) -> Option<PlaybackState> {
    if !settings.enabled {
        return None;
    }
    Some(match state {
        TransportStatus::Playing | TransportStatus::Buffering => PlaybackState::Playing,
        TransportStatus::Paused if settings.show_paused => PlaybackState::Paused,
        TransportStatus::Stopped
        | TransportStatus::Resolving
        | TransportStatus::Paused
        | TransportStatus::Failed => return None,
    })
}

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
        let client_id = activity.settings.client_id.as_str();
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
                "activity": activity.map(activity_json),
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

fn activity_json(activity: &Activity) -> Value {
    let track = &activity.media.track;
    let mut value = json!({
        "details": discord_text(&track.title, "Idle"),
        "state": discord_text(&track.artist, "Unknown artist"),
        "assets": {
            "large_image": activity.large_image,
            "large_text": discord_text(&track.album, "Unknown album"),
        },
        "timestamps": {},
        "instance": false,
        "status_display_type": status_display_type(activity.settings.display_type),
        "type": if activity.settings.show_as_listening { 2 } else { 0 },
    });
    if let Some(start) = activity.started_at_millis {
        value["timestamps"]["start"] = json!(start / 1_000);
    }
    if let Some(end) = activity.ended_at_millis {
        value["timestamps"]["end"] = json!(end / 1_000);
    }
    let (details_url, state_url) = activity_urls(activity);
    if let Some(details_url) = details_url {
        value["details_url"] = json!(details_url);
    }
    if let Some(state_url) = state_url {
        value["state_url"] = json!(state_url);
    }
    value
}

const fn status_display_type(display_type: DisplayType) -> u8 {
    match display_type {
        DisplayType::Application => 0,
        DisplayType::Artist => 1,
        DisplayType::Song => 2,
    }
}

fn activity_urls(activity: &Activity) -> (Option<String>, Option<String>) {
    let track = &activity.media.track;
    let track_artist = track.artist.trim();
    let album_artist = activity
        .media
        .track
        .album_display_artist
        .as_deref()
        .map(str::trim)
        .filter(|artist| !artist.is_empty())
        .unwrap_or(track_artist);
    let mut details = None;
    let mut state = None;
    if matches!(
        activity.settings.link_type,
        LinkType::LastFm | LinkType::MusicBrainzLastFm
    ) {
        state = lastfm_artist_url(track_artist);
        details = lastfm_track_url(album_artist, &track.album, &track.title);
    }
    if matches!(
        activity.settings.link_type,
        LinkType::MusicBrainz | LinkType::MusicBrainzLastFm
    ) {
        if activity.settings.link_type == LinkType::MusicBrainz {
            state = track
                .primary_artist_musicbrainz_id
                .as_deref()
                .and_then(|id| musicbrainz_url("artist", id));
        }
        details = track
            .musicbrainz_release_track_id
            .as_deref()
            .and_then(|id| musicbrainz_url("track", id))
            .or_else(|| {
                track
                    .musicbrainz_recording_id
                    .as_deref()
                    .and_then(|id| musicbrainz_url("recording", id))
            })
            .or(details);
    }
    (details, state)
}

fn lastfm_artist_url(artist: &str) -> Option<String> {
    let artist = artist.trim();
    (!artist.is_empty()).then(|| format!("https://www.last.fm/music/{}", encode_segment(artist)))
}

fn lastfm_track_url(artist: &str, album: &str, title: &str) -> Option<String> {
    let artist = artist.trim();
    let title = title.trim();
    if artist.is_empty() || title.is_empty() {
        return None;
    }
    let album = if album.trim().is_empty() { "_" } else { album };
    let url = format!(
        "https://www.last.fm/music/{}/{}/{}",
        encode_segment(artist),
        encode_segment(album),
        encode_segment(title)
    );
    (url.len() <= MAX_URL_LENGTH).then_some(url)
}

fn musicbrainz_url(entity: &str, id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    Some(format!(
        "https://musicbrainz.org/{entity}/{}",
        encode_segment(id)
    ))
}

fn discord_text(value: &str, fallback: &str) -> String {
    let text = if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    };
    let mut text = text.chars().take(MAX_TEXT_LENGTH).collect::<String>();
    if text.chars().count() < 2 {
        text.push(' ');
    }
    text
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            b' ' => encoded.push_str("%20"),
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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

    #[test]
    fn lastfm_album_url_uses_the_album_display_artist() {
        let view = super::super::tests::test_view(1, "Album", TransportStatus::Playing, 0);
        let activity = Activity {
            settings: Settings {
                link_type: LinkType::LastFm,
                ..Settings::default()
            },
            media: view.transport.current.expect("current media"),
            playback_state: PlaybackState::Playing,
            started_at_millis: None,
            ended_at_millis: None,
            large_image: APP_ICON_URL.to_string(),
        };

        let (details, state) = activity_urls(&activity);
        assert_eq!(
            details.as_deref(),
            Some("https://www.last.fm/music/Album%20Artist/Album/Track")
        );
        assert_eq!(state.as_deref(), Some("https://www.last.fm/music/Artist"));
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
        let activity = Activity {
            settings: Settings {
                client_id: "client".to_string(),
                ..Settings::default()
            },
            media: view.transport.current.expect("current media"),
            playback_state: PlaybackState::Playing,
            started_at_millis: None,
            ended_at_millis: None,
            large_image: APP_ICON_URL.to_string(),
        };

        assert!(connection.apply(Some(&activity)));
        assert!(connection.stream.is_none());
        assert!(connection.client_id.is_none());
    }
}
