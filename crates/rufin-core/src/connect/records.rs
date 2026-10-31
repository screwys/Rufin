//! What each record kind encodes from the Store and settings, the diff that records this
//! device's changes, and the apply of rows other devices wrote. A row holds its value only
//! for kinds that live nowhere else; the others are encoded again when a row leaves.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read as _, Write as _},
    sync::{Arc, Mutex, Weak},
};

use base64::Engine as _;
use futures_util::future::BoxFuture;
use library::{Change, Scope, SourceId};
use rufin_connect::store::{self, Cursor, Row, Select, Store};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{ConnectOwner, error, session::SessionValue};
use crate::settings::{
    ConfiguredSource, load_provider_secret, read_scrobbling, seal_credential, store_scrobbling,
};

const PAGE: usize = 512;
/// Received rows apply kind by kind in this order, so parents come before their children.
const ORDER: [&str; 14] = [
    "device",
    "setting",
    "folder",
    "playlist",
    "smart",
    "artwork",
    "entry",
    "state",
    "listen",
    "legacy",
    "analysis",
    "album-loudness",
    "session",
    "queue",
];
/// Kinds a full pass compares item by item, and whether an item that is gone leaves a
/// tombstone. Listens and analysis results are compared by id: their values do not change
/// once recorded, and reading every waveform at each start costs too much.
const COMPARED: [(&str, bool); 8] = [
    ("state", true),
    ("legacy", true),
    ("folder", true),
    ("playlist", true),
    ("entry", true),
    ("smart", true),
    ("artwork", true),
    // A cleared cache is not a deletion.
    ("album-loudness", false),
];
/// Kinds whose rows hold their value.
const BODIES: [&str; 3] = ["device", "session", "queue"];
const PREFERENCES: [&str; 5] = [
    "prefer_distinct_track_covers",
    "use_sort_tags",
    "external_metadata_enabled",
    "sidebar.pins",
    "lyrics.suppressed_auto_lyrics",
];
const SCROBBLING: [&str; 3] = ["lastfm", "librefm", "listenbrainz"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct DeviceValue {
    pub(super) name: String,
    pub(super) trusted: bool,
    /// When the device was last paired, unix milliseconds. Its own records carry it, so a
    /// record from an earlier membership cannot trust it again.
    pub(super) paired: i64,
}

/// One encoded item: its record id and parent, the digest the diff compares and the value
/// that leaves the device. A held item keeps its record as it is and sends no value.
pub(super) struct Item {
    id: String,
    parent: Option<String>,
    digest: String,
    value: Value,
    held: bool,
}

impl Item {
    fn new(id: String, parent: Option<String>, value: &impl Serialize) -> Result<Self, String> {
        Self::digested(id, parent, value, value)
    }

    /// An item whose digest covers only what must be equal on every device.
    pub(super) fn digested(
        id: String,
        parent: Option<String>,
        value: &impl Serialize,
        digest: &impl Serialize,
    ) -> Result<Self, String> {
        Ok(Self {
            id,
            parent,
            digest: store::digest(digest).map_err(error)?,
            value: serde_json::to_value(value).map_err(error)?,
            held: false,
        })
    }

    fn held(id: String) -> Self {
        Self {
            id,
            parent: None,
            digest: String::new(),
            value: Value::Null,
            held: true,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StateValue {
    favorite: Option<bool>,
    rating: Option<i64>,
    changed_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct LegacyValue {
    play_count: i64,
    skip_count: i64,
    last_played_at: Option<i64>,
    title: String,
    artist: String,
    album: String,
}

#[derive(Serialize, Deserialize)]
struct PlaylistValue {
    name: String,
    folder: Option<String>,
    created_at: i64,
    mosaic: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct EntryValue {
    playlist: String,
    rank: String,
    uri: String,
    copy: Copy,
}

/// What a track shows where it is not in the Store.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Copy {
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) album: String,
    pub(super) duration_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct FolderValue {
    name: String,
    smart: bool,
}

#[derive(Serialize, Deserialize)]
struct SmartValue {
    name: String,
    folder: Option<String>,
    definition: String,
    mosaic: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct ArtworkValue {
    mime: Option<String>,
    revision: String,
    bytes: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct AnalysisValue {
    loudness: Option<(Option<f64>, Option<f64>)>,
    waveform: Option<Vec<(f64, f64)>>,
    fingerprint: Option<FingerprintValue>,
}

#[derive(Serialize, Deserialize)]
struct FingerprintValue {
    version: String,
    value: String,
    duration_s: u64,
}

#[derive(Serialize, Deserialize)]
struct LoudnessValue {
    lufs: Option<f64>,
    true_peak: Option<f64>,
}

/// A source or integration. A Local source shares neither name nor payload: its roots stay
/// on the device it belongs to.
#[derive(Serialize, Deserialize)]
struct SourceValue {
    kind: String,
    name: Option<String>,
    payload: Option<String>,
    music_folder_id: Option<String>,
    half_stars: bool,
    owner: Option<String>,
    secret: Option<String>,
}

/// A scrobbling account: its settings, and its secrets by kind as JSON in `secret`.
#[derive(Serialize, Deserialize)]
struct ScrobblingValue {
    account: Value,
    secret: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RelayValue {
    relay: Option<String>,
    public_relay: bool,
}

/// Rows of one pull or one folder read that wait for their parent, by the device they came
/// from.
#[derive(Default)]
pub(super) struct Receiving {
    held: Vec<(String, Row)>,
}

pub(super) struct Records {
    owner: Weak<ConnectOwner>,
    pub(super) store: Store,
    pub(super) device: String,
    key: age::x25519::Identity,
    pub(super) catalog: Arc<library::Catalog>,
    /// Diff passes and applies run one at a time, so the pass after an apply finds its rows.
    work: tokio::sync::Mutex<()>,
    devices: Mutex<BTreeMap<String, DeviceValue>>,
    /// The stored session records by device, this one included.
    pub(super) sessions: tokio::sync::watch::Sender<BTreeMap<String, SessionValue>>,
}

impl Records {
    pub(super) fn new(
        owner: &Arc<ConnectOwner>,
        store: Store,
        device: String,
        key: age::x25519::Identity,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner: Arc::downgrade(owner),
            store,
            device,
            key,
            catalog: Arc::clone(&owner.catalog),
            work: tokio::sync::Mutex::new(()),
            devices: Mutex::new(BTreeMap::new()),
            sessions: tokio::sync::watch::channel(BTreeMap::new()).0,
        })
    }

    pub(super) fn owner(&self) -> Result<Arc<ConnectOwner>, String> {
        self.owner
            .upgrade()
            .ok_or_else(|| "Connect stopped".to_owned())
    }

    pub(super) fn key_text(&self) -> String {
        use age::secrecy::ExposeSecret;
        self.key.to_string().expose_secret().to_owned()
    }

    /// Encrypts to the profile key.
    pub(super) fn encrypt(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let recipient = self.key.to_public();
        let mut output =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .map_err(error)?
                .wrap_output(Vec::new())
                .map_err(error)?;
        output.write_all(bytes).map_err(error)?;
        output.finish().map_err(error)
    }

    pub(super) fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = age::Decryptor::new(bytes)
            .map_err(error)?
            .decrypt(std::iter::once(&self.key as &dyn age::Identity))
            .map_err(error)?;
        let mut plain = Vec::new();
        input.read_to_end(&mut plain).map_err(error)?;
        Ok(plain)
    }

    pub(super) async fn load_devices(&self) -> Result<(), String> {
        let devices = self
            .store
            .bodies("device")
            .await
            .map_err(error)?
            .into_iter()
            .filter_map(|row| {
                let value = serde_json::from_value(row.value?).ok()?;
                Some((row.id.strip_prefix("device:")?.to_owned(), value))
            })
            .collect();
        *self.devices.lock().unwrap_or_else(|p| p.into_inner()) = devices;
        Ok(())
    }

    /// This device's record as it writes it: its name, trusted, paired when it last joined.
    fn own(&self) -> Result<DeviceValue, String> {
        let paired = self
            .devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&self.device)
            .map_or(0, |value| value.paired);
        Ok(DeviceValue {
            name: self.owner()?.device_config().name,
            trusted: true,
            paired,
        })
    }

    pub(super) fn add_device(&self, device: &str, value: DeviceValue) {
        self.devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(device.to_owned(), value);
    }

    /// Every device of the profile and whether it is trusted, this one included.
    pub(super) fn trusted(&self) -> HashMap<String, bool> {
        let mut devices = self
            .devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(device, value)| (device.clone(), value.trusted))
            .collect::<HashMap<_, _>>();
        devices.insert(self.device.clone(), true);
        devices
    }

    /// The other trusted devices by name.
    pub(super) fn devices(&self) -> BTreeMap<String, String> {
        self.devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(device, value)| value.trusted && **device != self.device)
            .map(|(device, value)| (device.clone(), value.name.clone()))
            .collect()
    }

    pub(super) fn peers(&self) -> Vec<String> {
        self.devices().into_keys().collect()
    }

    /// The sequence of `peer`'s records this device applied up to.
    pub(super) async fn pulled(&self, peer: &str) -> Result<i64, String> {
        Ok(self
            .store
            .peers()
            .await
            .map_err(error)?
            .into_iter()
            .find(|row| row.device == peer)
            .map_or(0, |row| row.pulled))
    }

    /// Records what this device knows of `device`.
    pub(super) async fn write_device(&self, device: &str) -> Result<Option<i64>, String> {
        let _work = self.work.lock().await;
        let changes = self.device_changes(device).await?;
        self.write(changes, None).await
    }

    async fn device_changes(&self, device: &str) -> Result<Vec<store::Change>, String> {
        let value = self
            .devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(device)
            .cloned()
            .ok_or("The device is not paired")?;
        let id = format!("device:{device}");
        let item = Item::new(id.clone(), None, &value)?;
        self.diff(vec![item], Select::Ids(std::slice::from_ref(&id)), false)
            .await
    }

    /// Takes a device's access away: its record says it is no longer trusted.
    pub(super) async fn remove_device(&self, device: &str) -> Result<Option<i64>, String> {
        {
            let mut devices = self.devices.lock().unwrap_or_else(|p| p.into_inner());
            let Some(value) = devices.get_mut(device).filter(|value| value.trusted) else {
                return Ok(None);
            };
            value.trusted = false;
        }
        self.write_device(device).await
    }

    /// This device leaves the profile, and its session and queue with it.
    pub(super) async fn leave(&self) -> Result<Option<i64>, String> {
        let own = DeviceValue {
            trusted: false,
            ..self.own()?
        };
        self.add_device(&self.device, own);
        let _work = self.work.lock().await;
        let mut changes = self.device_changes(&self.device).await?;
        let playback = [
            format!("session:{}", self.device),
            format!("queue:{}", self.device),
        ];
        changes.extend(self.diff(Vec::new(), Select::Ids(&playback), true).await?);
        self.write(changes, None).await
    }

    async fn write(
        &self,
        changes: Vec<store::Change>,
        listened: Option<i64>,
    ) -> Result<Option<i64>, String> {
        if changes.is_empty() && listened.is_none() {
            return Ok(None);
        }
        let sessions = changes
            .iter()
            .filter(|change| kind(&change.id) == "session")
            .map(|change| (change.id.clone(), change.value.clone()))
            .collect::<Vec<_>>();
        let sequence = self.store.write(changes, listened).await.map_err(error)?;
        self.remember_sessions(sessions);
        Ok(sequence)
    }

    /// Compares encoded items with the stored records. A stored live record without an item
    /// becomes a tombstone when `tombstones` is set.
    pub(super) async fn diff(
        &self,
        items: Vec<Item>,
        select: Select<'_>,
        tombstones: bool,
    ) -> Result<Vec<store::Change>, String> {
        let stored = self.store.digests(select).await.map_err(error)?;
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for item in items {
            seen.insert(item.id.clone());
            if !item.held && stored.get(&item.id) != Some(&item.digest) {
                let body = BODIES.contains(&kind(&item.id));
                changes.push(store::Change {
                    id: item.id,
                    parent: item.parent,
                    digest: item.digest,
                    deleted: false,
                    value: body.then_some(item.value),
                });
            }
        }
        if tombstones {
            for (id, digest) in stored {
                if !digest.is_empty() && !seen.contains(&id) {
                    changes.push(store::Change {
                        id,
                        parent: None,
                        digest: String::new(),
                        deleted: true,
                        value: None,
                    });
                }
            }
        }
        Ok(changes)
    }

    async fn compare(&self, kind: &str, tombstones: bool) -> Result<Vec<store::Change>, String> {
        let items = self.items(kind, None).await?;
        self.diff(items, Select::Kind(kind), tombstones).await
    }

    /// Compares the items of these record ids; an id whose item is gone is a tombstone.
    async fn compare_ids(
        &self,
        kind: &str,
        objects: Vec<String>,
    ) -> Result<Vec<store::Change>, String> {
        let ids = objects
            .iter()
            .map(|object| format!("{kind}:{object}"))
            .collect::<Vec<_>>();
        let items = self.items(kind, Some(&objects)).await?;
        self.diff(items, Select::Ids(&ids), kind != "analysis")
            .await
    }

    /// One diff pass: everything when `changes` is None, otherwise what they name.
    pub(super) async fn pass(
        &self,
        changes: Option<&[Arc<Change>]>,
    ) -> Result<Option<i64>, String> {
        let _work = self.work.lock().await;
        let scopes = changes
            .map(|changes| {
                changes
                    .iter()
                    .flat_map(|change| change.scopes.iter())
                    .collect::<Vec<_>>()
            })
            .filter(|scopes| !scopes.iter().any(|scope| matches!(scope, Scope::Reload)));
        let Some(scopes) = scopes else {
            let mut written = self.settings_changes().await?;
            for (kind, tombstones) in COMPARED {
                written.extend(self.compare(kind, tombstones).await?);
            }
            let (listens, listened) = self.unrecorded_listens().await?;
            written.extend(listens);
            written.extend(self.unrecorded_analysis().await?);
            written.extend(self.session_changes().await?);
            return self.write(written, listened).await;
        };
        let mut kinds = Vec::new();
        let mut states = Vec::new();
        let mut entries = Vec::new();
        let mut hashes = Vec::new();
        let mut heard = false;
        let mut queue = false;
        for scope in scopes {
            match scope {
                Scope::State { items } => {
                    states.extend(items.iter().map(|item| match &item.target {
                        library::FavoriteTarget::Track(uri)
                        | library::FavoriteTarget::Album(uri)
                        | library::FavoriteTarget::Artist(uri) => uri.clone(),
                    }))
                }
                Scope::Listened { .. } => heard = true,
                Scope::Playlist {
                    key,
                    entries: changed,
                } => {
                    kinds.extend(["playlist", "artwork"]);
                    if *changed {
                        entries.push(*key);
                    }
                }
                // Playlists were added or removed, and their entries with them.
                Scope::Playlists => kinds.extend(["playlist", "entry", "artwork"]),
                Scope::Smart { .. } => kinds.extend(["smart", "artwork"]),
                Scope::Folders => kinds.extend(["folder", "smart"]),
                Scope::Analysis { hash } => hashes.push(*hash),
                Scope::Queue => queue = true,
                _ => {}
            }
        }
        kinds.sort_unstable();
        kinds.dedup();
        let mut written = Vec::new();
        for kind in kinds {
            written.extend(self.compare(kind, true).await?);
        }
        if !states.is_empty() {
            let portable = self.portable(&states).await?;
            let ids = states
                .iter()
                .map(|uri| format!("state:{}", portable.get(uri).unwrap_or(uri)))
                .collect::<Vec<_>>();
            let items = self.state_items(Some(&states), Some(portable)).await?;
            written.extend(self.diff(items, Select::Ids(&ids), true).await?);
        }
        for key in entries {
            written.extend(self.playlist_entry_changes(key).await?);
        }
        if !hashes.is_empty() {
            let objects = hashes.iter().map(|hash| hex(hash)).collect::<Vec<_>>();
            let items = self.items("analysis", Some(&objects)).await?;
            // A hash without results names an album's members key.
            if items.len() < objects.len() {
                written.extend(self.compare("album-loudness", false).await?);
            }
            let ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
            written.extend(self.diff(items, Select::Ids(&ids), false).await?);
        }
        let mut listened = None;
        if heard {
            let (listens, last) = self.heard_listens().await?;
            written.extend(listens);
            listened = last;
        }
        if queue {
            written.extend(self.session_changes().await?);
        }
        self.write(written, listened).await
    }

    /// The setting records and this device's record, after a settings change.
    pub(super) async fn settings_pass(&self) -> Result<Option<i64>, String> {
        let _work = self.work.lock().await;
        let changes = self.settings_changes().await?;
        self.write(changes, None).await
    }

    async fn settings_changes(&self) -> Result<Vec<store::Change>, String> {
        let mut changes = self.compare("setting", true).await?;
        let own = self.own()?;
        self.add_device(&self.device, own.clone());
        let id = format!("device:{}", self.device);
        changes.extend(
            self.diff(
                vec![Item::new(id.clone(), None, &own)?],
                Select::Ids(std::slice::from_ref(&id)),
                false,
            )
            .await?,
        );
        Ok(changes)
    }

    async fn playlist_entry_changes(
        &self,
        key: library::PlaylistKey,
    ) -> Result<Vec<store::Change>, String> {
        let keys = [key];
        let Some(playlist) = self
            .catalog
            .playlist_copies(Some(&keys))
            .await
            .map_err(error)?
            .pop()
        else {
            return Ok(Vec::new());
        };
        let items = self.entry_items(Some(&keys), None).await?;
        let parent = format!("playlist:{}", playlist.object_id);
        self.diff(items, Select::Parent(&parent), true).await
    }

    /// Listens without a record, and tombstones for records of listens that are gone.
    async fn unrecorded_listens(&self) -> Result<(Vec<store::Change>, Option<i64>), String> {
        let listens = self.catalog.listen_ids().await.map_err(error)?;
        let listened = self
            .store
            .profile()
            .await
            .map_err(error)?
            .map(|row| row.listened);
        let last = listens
            .iter()
            .map(|(_, key)| *key)
            .max()
            .filter(|last| Some(*last) != listened);
        let stored = self
            .store
            .digests(Select::Kind("listen"))
            .await
            .map_err(error)?;
        let present = listens
            .iter()
            .map(|(id, _)| format!("listen:{id}"))
            .collect::<HashSet<_>>();
        let missing = listens
            .into_iter()
            .filter(|(id, _)| !stored.contains_key(&format!("listen:{id}")))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let gone = stored
            .into_iter()
            .filter(|(id, digest)| !digest.is_empty() && !present.contains(id))
            .filter_map(|(id, _)| id.strip_prefix("listen:").map(str::to_owned))
            .collect::<Vec<_>>();
        let mut changes = Vec::new();
        for page in missing.chunks(PAGE).chain(gone.chunks(PAGE)) {
            changes.extend(self.compare_ids("listen", page.to_vec()).await?);
        }
        Ok((changes, last))
    }

    /// Listens recorded since the last pass, and a few before: a listen can be written again
    /// with a longer listening time.
    async fn heard_listens(&self) -> Result<(Vec<store::Change>, Option<i64>), String> {
        let profile = self.store.profile().await.map_err(error)?;
        let mut after = profile.map_or(0, |row| row.listened).saturating_sub(16);
        let mut changes = Vec::new();
        let mut last = None;
        loop {
            let listens = self
                .catalog
                .listens_after(library::ListenKey::from_raw(after), PAGE)
                .await
                .map_err(error)?;
            let Some(final_key) = listens.last().map(|listen| listen.listen_key) else {
                break;
            };
            after = final_key;
            last = Some(final_key);
            let items = self.encode_listens(listens).await?;
            let ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
            changes.extend(self.diff(items, Select::Ids(&ids), false).await?);
        }
        Ok((changes, last))
    }

    async fn unrecorded_analysis(&self) -> Result<Vec<store::Change>, String> {
        let stored = self
            .store
            .digests(Select::Kind("analysis"))
            .await
            .map_err(error)?;
        let missing = self
            .catalog
            .analysis_hashes()
            .await
            .map_err(error)?
            .iter()
            .map(|hash| hex(hash))
            .filter(|hash| !stored.contains_key(&format!("analysis:{hash}")))
            .collect::<Vec<_>>();
        let mut changes = Vec::new();
        for page in missing.chunks(PAGE) {
            changes.extend(self.compare_ids("analysis", page.to_vec()).await?);
        }
        Ok(changes)
    }

    /// The items of a kind: all of them, or those of these record ids without their kind.
    /// Images are read only for ids; a diff compares their revisions.
    async fn items(&self, kind: &str, objects: Option<&[String]>) -> Result<Vec<Item>, String> {
        let mut items = match kind {
            "state" => match objects {
                Some(objects) => {
                    let local = self.local(objects).await?;
                    let uris = objects
                        .iter()
                        .map(|uri| local.get(uri).unwrap_or(uri).clone())
                        .collect::<Vec<_>>();
                    let named = local.into_iter().map(|(portable, uri)| (uri, portable));
                    self.state_items(Some(&uris), Some(named.collect())).await?
                }
                None => self.state_items(None, None).await?,
            },
            "listen" => match objects {
                Some(ids) => {
                    let listens = self.catalog.listens_by_id(ids).await.map_err(error)?;
                    self.encode_listens(listens).await?
                }
                None => Vec::new(),
            },
            "legacy" => {
                let keys = objects
                    .map(|keys| {
                        keys.iter()
                            .map(|key| legacy_key(key))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?;
                self.legacy_items(keys.as_deref()).await?
            }
            "folder" => self.folder_items().await?,
            "playlist" => self.playlist_items().await?,
            "entry" => self.entry_items(None, objects).await?,
            "smart" => self.smart_items().await?,
            "artwork" => self.artwork_items(objects).await?,
            "analysis" => {
                let hashes = objects
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|hash| unhex(hash).ok()?.try_into().ok())
                    .collect::<Vec<[u8; 32]>>();
                self.analysis_items(&hashes).await?
            }
            "album-loudness" => self.album_loudness_items().await?,
            "setting" => self.settings_items().await?,
            _ => Vec::new(),
        };
        if let Some(objects) = objects {
            let wanted = objects
                .iter()
                .map(|object| format!("{kind}:{object}"))
                .collect::<HashSet<_>>();
            items.retain(|item| wanted.contains(&item.id));
        }
        Ok(items)
    }

    /// Track URIs by their portable form: a track of one of this device's Local sources is
    /// named as the peer source names it on other devices.
    pub(super) async fn portable(
        &self,
        uris: &[String],
    ) -> Result<HashMap<String, String>, String> {
        let local = self.local_sources()?;
        if local.is_empty() || uris.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(self
            .catalog
            .track_objects(uris)
            .await
            .map_err(error)?
            .into_iter()
            .filter(|(_, source, _)| local.contains(source))
            .map(|(uri, source, object)| {
                (uri, library::source_entity_uri(&source, "track", &object))
            })
            .collect())
    }

    async fn portable_all(&self, uris: Vec<String>) -> Result<Vec<String>, String> {
        let portable = self.portable(&uris).await?;
        Ok(uris
            .into_iter()
            .map(|uri| portable.get(&uri).cloned().unwrap_or(uri))
            .collect())
    }

    /// This device's URI of each portable URI that names a track of its own Local sources.
    async fn local(&self, uris: &[String]) -> Result<HashMap<String, String>, String> {
        let local = self.local_sources()?;
        let mut named = HashMap::new();
        for uri in uris {
            if let Some((source, kind, object)) = library::source_entity_parts(uri)
                && kind == "track"
                && local.contains(&source)
            {
                named.insert((source, object), uri.clone());
            }
        }
        let objects = named.keys().cloned().collect::<Vec<_>>();
        Ok(self
            .catalog
            .track_uris(&objects)
            .await
            .map_err(error)?
            .into_iter()
            .filter_map(|(source, object, track)| Some((named.remove(&(source, object))?, track)))
            .collect())
    }

    pub(super) async fn local_all(&self, uris: &[String]) -> Result<Vec<String>, String> {
        let local = self.local(uris).await?;
        Ok(uris
            .iter()
            .map(|uri| local.get(uri).cloned().unwrap_or_else(|| uri.clone()))
            .collect())
    }

    fn local_sources(&self) -> Result<HashSet<SourceId>, String> {
        Ok(self
            .owner()?
            .settings
            .load()
            .sources
            .configured
            .iter()
            .filter(|source| source.configuration.is_local())
            .map(|source| source.configuration.source_id.clone())
            .collect())
    }

    /// Favorites and ratings Rufin holds. One that waits for its server delivery is held: the
    /// delivery settles whether it stays Rufin's. `portable` names the requested URIs when the
    /// caller already looked them up.
    async fn state_items(
        &self,
        uris: Option<&[String]>,
        portable: Option<HashMap<String, String>>,
    ) -> Result<Vec<Item>, String> {
        let states = self.catalog.user_states(uris).await.map_err(error)?;
        let portable = match portable {
            Some(portable) => portable,
            None => {
                self.portable(
                    &states
                        .iter()
                        .map(|(state, _)| state.media_uri.clone())
                        .collect::<Vec<_>>(),
                )
                .await?
            }
        };
        states
            .into_iter()
            .map(|(state, delivering)| {
                let uri = portable.get(&state.media_uri).unwrap_or(&state.media_uri);
                if delivering {
                    return Ok(Item::held(format!("state:{uri}")));
                }
                // Two devices that set the same value at different times agree.
                Item::digested(
                    format!("state:{uri}"),
                    None,
                    &StateValue {
                        favorite: state.favorite,
                        rating: state.rating,
                        changed_at: state.changed_at,
                    },
                    &(state.favorite, state.rating),
                )
            })
            .collect()
    }

    async fn encode_listens(
        &self,
        listens: Vec<library::ActivityRecord>,
    ) -> Result<Vec<Item>, String> {
        let uris = listens
            .iter()
            .flat_map(|listen| {
                std::iter::once(listen.listen.media_uri.clone())
                    .chain(listen.current_media_uri.clone())
            })
            .collect::<Vec<_>>();
        let portable = self.portable(&uris).await?;
        let map = |uri: &String| portable.get(uri).unwrap_or(uri).clone();
        listens
            .into_iter()
            .filter_map(|mut listen| {
                let id = listen.listen.external_id.clone()?;
                listen.listen_key = 0;
                listen.listen.media_uri = map(&listen.listen.media_uri);
                listen.current_media_uri = listen.current_media_uri.as_ref().map(map);
                Some(Item::new(format!("listen:{id}"), None, &listen))
            })
            .collect()
    }

    async fn legacy_items(
        &self,
        keys: Option<&[library::LegacyActivityKey]>,
    ) -> Result<Vec<Item>, String> {
        self.catalog
            .legacy_activity(keys)
            .await
            .map_err(error)?
            .into_iter()
            .map(|record| {
                Item::new(
                    format!(
                        "legacy:{}/{}/{}/{}",
                        record.source_id, record.period, record.item_kind, record.track_object_id
                    ),
                    None,
                    &LegacyValue {
                        play_count: record.play_count,
                        skip_count: record.skip_count,
                        last_played_at: record.last_played_at,
                        title: record.title,
                        artist: record.artist,
                        album: record.album,
                    },
                )
            })
            .collect()
    }

    async fn folder_items(&self) -> Result<Vec<Item>, String> {
        self.catalog
            .playlist_folder_copies()
            .await
            .into_iter()
            .map(|folder| {
                Item::new(
                    format!("folder:{}", folder.object_id),
                    None,
                    &FolderValue {
                        name: folder.name,
                        smart: folder.smart,
                    },
                )
            })
            .collect()
    }

    async fn playlist_items(&self) -> Result<Vec<Item>, String> {
        let mut items = Vec::new();
        for playlist in self.catalog.playlist_copies(None).await.map_err(error)? {
            let mosaic = self.portable_mosaic(playlist.mosaic.as_deref()).await?;
            items.push(Item::new(
                format!("playlist:{}", playlist.object_id),
                playlist
                    .folder
                    .as_ref()
                    .map(|folder| format!("folder:{folder}")),
                &PlaylistValue {
                    name: playlist.name,
                    folder: playlist.folder,
                    created_at: playlist.created_at,
                    mosaic,
                },
            )?);
        }
        Ok(items)
    }

    async fn portable_mosaic(&self, mosaic: Option<&str>) -> Result<Option<Vec<String>>, String> {
        match mosaic {
            Some(mosaic) => Ok(Some(
                self.portable_all(serde_json::from_str(mosaic).map_err(error)?)
                    .await?,
            )),
            None => Ok(None),
        }
    }

    async fn local_mosaic(&self, mosaic: Option<Vec<String>>) -> Result<Option<String>, String> {
        match mosaic {
            Some(uris) => serde_json::to_string(&self.local_all(&uris).await?)
                .map(Some)
                .map_err(error),
            None => Ok(None),
        }
    }

    /// Entries of Rufin playlists. Their digest leaves out the track's display copy, which
    /// differs between devices.
    async fn entry_items(
        &self,
        playlists: Option<&[library::PlaylistKey]>,
        entries: Option<&[String]>,
    ) -> Result<Vec<Item>, String> {
        let copies = self
            .catalog
            .playlist_entry_copies(playlists, entries)
            .await
            .map_err(error)?;
        let portable = self
            .portable(
                &copies
                    .iter()
                    .map(|copy| copy.track.media_uri.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;
        copies
            .into_iter()
            .map(|copy| {
                let value = EntryValue {
                    rank: hex(&copy.rank),
                    uri: portable
                        .get(&copy.track.media_uri)
                        .cloned()
                        .unwrap_or(copy.track.media_uri),
                    copy: Copy {
                        title: copy.track.metadata.title,
                        artist: copy.track.metadata.artist,
                        album: copy.track.metadata.album,
                        duration_ms: copy.track.metadata.duration_millis,
                    },
                    playlist: copy.playlist,
                };
                Item::digested(
                    format!("entry:{}", copy.object_id),
                    Some(format!("playlist:{}", value.playlist)),
                    &value,
                    &(&value.playlist, &value.rank, &value.uri),
                )
            })
            .collect()
    }

    async fn smart_items(&self) -> Result<Vec<Item>, String> {
        let mut items = Vec::new();
        for smart in self.catalog.smart_playlist_copies().await.map_err(error)? {
            let mosaic = self.portable_mosaic(smart.mosaic.as_deref()).await?;
            items.push(Item::new(
                format!("smart:{}", smart.object_id),
                smart
                    .folder
                    .as_ref()
                    .map(|folder| format!("folder:{folder}")),
                &SmartValue {
                    name: smart.name,
                    folder: smart.folder,
                    definition: smart.definition,
                    mosaic,
                },
            )?);
        }
        Ok(items)
    }

    /// Images people chose for playlists. The digest covers the image's revision, a hash of
    /// its bytes.
    async fn artwork_items(&self, objects: Option<&[String]>) -> Result<Vec<Item>, String> {
        self.catalog
            .playlist_images(objects, objects.is_some())
            .await
            .map_err(error)?
            .into_iter()
            .map(|image| {
                let digest = (&image.mime, &image.revision);
                Item::digested(
                    format!("artwork:{}", image.object_id),
                    None,
                    &ArtworkValue {
                        mime: image.mime.clone(),
                        revision: image.revision.clone(),
                        bytes: image
                            .bytes
                            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
                    },
                    &digest,
                )
            })
            .collect()
    }

    async fn analysis_items(&self, hashes: &[[u8; 32]]) -> Result<Vec<Item>, String> {
        self.catalog
            .analysis(hashes)
            .await
            .map_err(error)?
            .into_iter()
            .map(|copy| {
                Item::new(
                    format!("analysis:{}", hex(&copy.hash)),
                    None,
                    &AnalysisValue {
                        loudness: copy.loudness,
                        waveform: copy.waveform,
                        fingerprint: copy.fingerprint.map(|row| FingerprintValue {
                            version: row.version,
                            value: row.fingerprint,
                            duration_s: row.duration_seconds,
                        }),
                    },
                )
            })
            .collect()
    }

    async fn album_loudness_items(&self) -> Result<Vec<Item>, String> {
        self.catalog
            .album_loudness_rows()
            .await
            .map_err(error)?
            .into_iter()
            .map(|(members, lufs, true_peak)| {
                Item::new(
                    format!("album-loudness:{members}"),
                    None,
                    &LoudnessValue { lufs, true_peak },
                )
            })
            .collect()
    }

    /// Source and integration configurations with their logins, the shared preferences,
    /// scrobbling accounts and the relay. Secrets are in the clear here; they are sealed when
    /// a value leaves the device. A digest covers a secret only as a hash keyed with the
    /// profile key, so no unkeyed hash of a login is stored or sent.
    async fn settings_items(&self) -> Result<Vec<Item>, String> {
        let owner = self.owner()?;
        let stored = owner.settings.load();
        let secrets = Arc::clone(&owner.secrets);
        let device = self.device.clone();
        let key = blake3::derive_key("Rufin Connect setting digests", self.key_text().as_bytes());
        let mut items = tokio::task::spawn_blocking(move || {
            let mut items = Vec::new();
            for (kind, sources) in [
                ("source", &stored.sources.configured),
                ("integration", &stored.sources.integrations),
            ] {
                for source in sources {
                    items.push(secret_item(
                        format!("setting:{kind}:{}", source.configuration.source_id),
                        &source_value(source, &device, secrets.as_ref())?,
                        &key,
                    )?);
                }
            }
            for name in PREFERENCES {
                items.push(Item::new(
                    format!("setting:preference:{name}"),
                    None,
                    &preference(&stored.ui, name)?,
                )?);
            }
            let scrobbling =
                serde_json::to_value(read_scrobbling(&stored, secrets.as_ref())?).map_err(error)?;
            for service in SCROBBLING {
                items.push(secret_item(
                    format!("setting:scrobbling:{service}"),
                    &scrobbling_value(&scrobbling, service)?,
                    &key,
                )?);
            }
            Ok::<_, String>(items)
        })
        .await
        .map_err(error)??;
        let config = owner.device_config();
        items.push(Item::new(
            "setting:relay".into(),
            None,
            &RelayValue {
                relay: config.relay,
                public_relay: config.public_relay,
            },
        )?);
        Ok(items)
    }

    /// Applies one page of received rows and stores those that applied, then moves the
    /// cursor of the device they came from. A row waiting for its parent is held; at `end`
    /// what still waits is settled.
    pub(super) async fn receive(
        &self,
        from: &str,
        mut rows: Vec<Row>,
        receiving: &mut Receiving,
        end: Option<i64>,
        cursor: impl for<'a> Fn(&'a str, i64) -> Cursor<'a>,
    ) -> Result<u64, String> {
        let _work = self.work.lock().await;
        let mut target = end.or(rows.iter().map(|row| row.sequence).max());
        for row in rows.iter_mut().filter(|row| kind(&row.id) == "setting") {
            if let Some(value) = &mut row.value {
                self.open(value)?;
            }
        }
        let mut pending = std::mem::take(&mut receiving.held);
        pending.extend(
            self.store
                .newer(rows)
                .await
                .map_err(error)?
                .into_iter()
                .map(|row| (from.to_owned(), row)),
        );
        let (applied, held) = self.apply(pending, end.is_some()).await?;
        receiving.held = held;
        if let Some(lowest) = receiving
            .held
            .iter()
            .filter(|(source, _)| source == from)
            .map(|(_, row)| row.sequence)
            .min()
        {
            target = target.map(|target| target.min(lowest - 1));
        }
        let count = applied.len() as u64;
        let applied = applied
            .into_iter()
            .map(|mut row| {
                if !BODIES.contains(&kind(&row.id)) {
                    row.value = None;
                }
                row
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            self.store
                .receive(&applied, cursor(from, target))
                .await
                .map_err(error)?;
            self.remember_sessions(
                applied
                    .into_iter()
                    .filter(|row| kind(&row.id) == "session")
                    .map(|row| (row.id, row.value.filter(|_| !row.deleted)))
                    .collect(),
            );
        }
        Ok(count)
    }

    /// Applies rows kind by kind; returns those that applied and those that wait for a parent.
    #[allow(clippy::type_complexity)]
    async fn apply(
        &self,
        rows: Vec<(String, Row)>,
        settle: bool,
    ) -> Result<(Vec<Row>, Vec<(String, Row)>), String> {
        let mut kinds = BTreeMap::<usize, Vec<(String, Row)>>::new();
        for (from, row) in rows {
            match ORDER.iter().position(|kind| *kind == self::kind(&row.id)) {
                Some(position) => kinds.entry(position).or_default().push((from, row)),
                None => tracing::warn!(id = row.id, "Connect record of an unknown kind"),
            }
        }
        let mut applied = Vec::new();
        let mut held = Vec::new();
        for (position, rows) in kinds {
            let kind = ORDER[position];
            let (ready, waiting) = self.parents(kind, rows, settle).await?;
            held.extend(waiting);
            let rows = ready.iter().map(|(_, row)| row).collect::<Vec<_>>();
            if self.apply_kind(kind, &rows).await.is_ok() {
                applied.extend(ready.into_iter().map(|(_, row)| row));
                continue;
            }
            // One failing row must not lose the others: the sender records an unchanged
            // value only once.
            for (_, row) in ready {
                match self.apply_kind(kind, &[&row]).await {
                    Ok(()) => applied.push(row),
                    Err(error) => {
                        tracing::warn!(%error, id = row.id, "Connect could not apply a record");
                    }
                }
            }
        }
        Ok((applied, held))
    }

    /// Splits rows into those that can apply now and those that wait for their parent. When
    /// settling, playlists apply without their folder and children of a gone playlist are
    /// dropped. A device that left or was removed cannot trust itself again; only a record of
    /// a later pairing does.
    #[allow(clippy::type_complexity)]
    async fn parents(
        &self,
        kind: &str,
        rows: Vec<(String, Row)>,
        settle: bool,
    ) -> Result<(Vec<(String, Row)>, Vec<(String, Row)>), String> {
        let present = match kind {
            "device" => {
                let devices = self.devices.lock().unwrap_or_else(|p| p.into_inner());
                let rows = rows
                    .into_iter()
                    .filter(|(_, row)| {
                        let device = row.id.strip_prefix("device:").unwrap_or_default();
                        let Some(value) = row.value.as_ref().and_then(|value| {
                            serde_json::from_value::<DeviceValue>(value.clone()).ok()
                        }) else {
                            return false;
                        };
                        !(value.trusted
                            && row.device == device
                            && devices.get(device).is_some_and(|stored| {
                                !stored.trusted && value.paired <= stored.paired
                            }))
                    })
                    .collect();
                return Ok((rows, Vec::new()));
            }
            "playlist" | "smart" => self
                .catalog
                .playlist_folder_copies()
                .await
                .into_iter()
                .map(|folder| folder.object_id)
                .collect::<HashSet<_>>(),
            "entry" | "artwork" => self
                .catalog
                .playlist_copies(None)
                .await
                .map_err(error)?
                .into_iter()
                .map(|playlist| playlist.object_id)
                .chain(
                    self.catalog
                        .smart_playlist_copies()
                        .await
                        .map_err(error)?
                        .into_iter()
                        .map(|smart| smart.object_id),
                )
                .collect::<HashSet<_>>(),
            _ => return Ok((rows, Vec::new())),
        };
        let parent = |row: &Row| -> Option<String> {
            let value = row.value.as_ref().filter(|_| !row.deleted)?;
            match kind {
                "playlist" | "smart" => value["folder"].as_str().map(str::to_owned),
                "entry" => value["playlist"].as_str().map(str::to_owned),
                _ => row.id.strip_prefix("artwork:").map(str::to_owned),
            }
        };
        let mut ready = Vec::new();
        let mut waiting = Vec::new();
        for (from, mut row) in rows {
            match parent(&row) {
                Some(parent) if !present.contains(&parent) => {
                    if !settle {
                        waiting.push((from, row));
                    } else if matches!(kind, "playlist" | "smart") {
                        if let Some(value) = &mut row.value {
                            value["folder"] = Value::Null;
                        }
                        ready.push((from, row));
                    } else {
                        tracing::debug!(id = row.id, "Connect record of a gone playlist");
                    }
                }
                _ => ready.push((from, row)),
            }
        }
        Ok((ready, waiting))
    }

    async fn apply_kind(&self, kind: &str, rows: &[&Row]) -> Result<(), String> {
        let live = rows.iter().filter(|row| !row.deleted).copied();
        let gone = |prefix: &str| {
            rows.iter()
                .filter(|row| row.deleted)
                .filter_map(|row| row.id.strip_prefix(prefix).map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let catalog = &self.catalog;
        match kind {
            "device" => self.apply_devices(rows),
            "setting" => self.apply_settings(rows).await,
            "folder" => {
                let folders = live
                    .map(|row| {
                        let value: FolderValue = decode(row)?;
                        Ok(library::PlaylistFolderCopy {
                            object_id: object(row, "folder:")?,
                            name: value.name,
                            smart: value.smart,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                catalog
                    .put_playlist_folders(&folders)
                    .await
                    .map_err(error)?;
                catalog
                    .delete_playlist_folders_by_object(&gone("folder:"))
                    .await
                    .map_err(error)
            }
            "playlist" => {
                let mut playlists = Vec::new();
                for row in live {
                    let value: PlaylistValue = decode(row)?;
                    playlists.push(library::PlaylistCopy {
                        object_id: object(row, "playlist:")?,
                        name: value.name,
                        folder: value.folder,
                        created_at: value.created_at,
                        mosaic: self.local_mosaic(value.mosaic).await?,
                    });
                }
                catalog
                    .put_playlist_copies(&playlists)
                    .await
                    .map_err(error)?;
                catalog
                    .delete_playlists_by_object(&gone("playlist:"))
                    .await
                    .map_err(error)
            }
            "smart" => {
                let mut playlists = Vec::new();
                for row in live {
                    let value: SmartValue = decode(row)?;
                    playlists.push(library::SmartPlaylistCopy {
                        object_id: object(row, "smart:")?,
                        name: value.name,
                        folder: value.folder,
                        definition: value.definition,
                        mosaic: self.local_mosaic(value.mosaic).await?,
                    });
                }
                catalog
                    .put_smart_playlist_copies(&playlists)
                    .await
                    .map_err(error)?;
                catalog
                    .delete_smart_playlists_by_object(&gone("smart:"))
                    .await
                    .map_err(error)
            }
            "artwork" => {
                let mut images = Vec::new();
                for row in rows {
                    let image = if row.deleted {
                        None
                    } else {
                        let value: ArtworkValue = decode(row)?;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(value.bytes.ok_or("The image is missing")?)
                            .map_err(error)?;
                        Some((bytes, value.mime))
                    };
                    images.push((object(row, "artwork:")?, image));
                }
                catalog.put_playlist_images(&images).await.map_err(error)
            }
            "entry" => {
                let values = live
                    .map(|row| Ok((object(row, "entry:")?, decode::<EntryValue>(row)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                let local = self
                    .local(
                        &values
                            .iter()
                            .map(|(_, value)| value.uri.clone())
                            .collect::<Vec<_>>(),
                    )
                    .await?;
                let mut entries = Vec::new();
                for (object_id, value) in values {
                    entries.push(library::PlaylistEntryCopy {
                        playlist: value.playlist,
                        object_id,
                        rank: unhex(&value.rank)?,
                        track: library::TrackReference::direct(
                            local.get(&value.uri).cloned().unwrap_or(value.uri),
                            value.copy.title,
                            value.copy.artist,
                            value.copy.album,
                            value.copy.duration_ms,
                        ),
                    });
                }
                catalog
                    .put_playlist_entries(&entries)
                    .await
                    .map_err(error)?;
                catalog
                    .remove_playlist_entries_by_object(&gone("entry:"))
                    .await
                    .map(|_| ())
                    .map_err(error)
            }
            "state" => {
                let uris = rows
                    .iter()
                    .map(|row| object(row, "state:"))
                    .collect::<Result<Vec<_>, String>>()?;
                let local = self.local(&uris).await?;
                let mut states = Vec::new();
                for (row, uri) in rows.iter().zip(uris) {
                    let value = match row.deleted {
                        true => StateValue {
                            favorite: None,
                            rating: None,
                            changed_at: None,
                        },
                        false => decode(row)?,
                    };
                    states.push(library::UserMediaStateWrite {
                        media_uri: local.get(&uri).cloned().unwrap_or(uri),
                        favorite: value.favorite,
                        rating: value.rating,
                        changed_at: value.changed_at,
                    });
                }
                catalog.put_user_states(&states).await.map_err(error)
            }
            "listen" => {
                let mut listens = live
                    .map(decode::<library::ActivityRecord>)
                    .collect::<Result<Vec<_>, String>>()?;
                let local = self
                    .local(
                        &listens
                            .iter()
                            .flat_map(|listen| {
                                std::iter::once(listen.listen.media_uri.clone())
                                    .chain(listen.current_media_uri.clone())
                            })
                            .collect::<Vec<_>>(),
                    )
                    .await?;
                let map = |uri: &mut String| {
                    if let Some(local) = local.get(uri) {
                        uri.clone_from(local);
                    }
                };
                for listen in &mut listens {
                    map(&mut listen.listen.media_uri);
                    if let Some(uri) = &mut listen.current_media_uri {
                        map(uri);
                    }
                }
                catalog.import_listens(&listens).await.map_err(error)?;
                catalog
                    .delete_listens(&gone("listen:"))
                    .await
                    .map_err(error)
            }
            "legacy" => {
                let mut records = Vec::new();
                for row in live {
                    let value: LegacyValue = decode(row)?;
                    let (source_id, period, item_kind, track_object_id) =
                        legacy_key(&object(row, "legacy:")?)?;
                    records.push(library::LegacyActivityRecord {
                        source_id,
                        period,
                        item_kind,
                        track_object_id,
                        play_count: value.play_count,
                        skip_count: value.skip_count,
                        last_played_at: value.last_played_at,
                        title: value.title,
                        artist: value.artist,
                        album: value.album,
                    });
                }
                catalog.put_legacy_activity(&records).await.map_err(error)?;
                let keys = gone("legacy:")
                    .iter()
                    .map(|key| legacy_key(key))
                    .collect::<Result<Vec<_>, String>>()?;
                catalog.delete_legacy_activity(&keys).await.map_err(error)
            }
            "analysis" => {
                let copies = live
                    .map(|row| {
                        let value: AnalysisValue = decode(row)?;
                        Ok(library::AnalysisCopy {
                            hash: unhex(&object(row, "analysis:")?)?
                                .try_into()
                                .map_err(|_| "Invalid audio hash")?,
                            loudness: value.loudness,
                            waveform: value.waveform,
                            fingerprint: value.fingerprint.map(|row| library::FingerprintRow {
                                version: row.version,
                                fingerprint: row.value,
                                duration_seconds: row.duration_s,
                            }),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                catalog.put_analysis_copies(&copies).await.map_err(error)
            }
            "album-loudness" => {
                let values = live
                    .map(|row| {
                        let value: LoudnessValue = decode(row)?;
                        Ok((object(row, "album-loudness:")?, value.lufs, value.true_peak))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                catalog
                    .put_album_loudness_rows(&values)
                    .await
                    .map_err(error)
            }
            _ => Ok(()),
        }
    }

    /// Device records set who is trusted; a record that removes this device turns Connect
    /// off.
    fn apply_devices(&self, rows: &[&Row]) -> Result<(), String> {
        let mut removed = false;
        {
            let mut devices = self.devices.lock().unwrap_or_else(|p| p.into_inner());
            for row in rows {
                let device = object(row, "device:")?;
                let value: DeviceValue = decode(row)?;
                removed |= device == self.device && !value.trusted;
                devices.insert(device, value);
            }
        }
        self.owner()?.devices_changed(removed);
        Ok(())
    }

    async fn apply_settings(&self, rows: &[&Row]) -> Result<(), String> {
        let owner = self.owner()?;
        let mut sources = Vec::new();
        let mut forgotten = Vec::new();
        for row in rows {
            let id = object(row, "setting:")?;
            let (group, name) = id.split_once(':').unwrap_or((id.as_str(), ""));
            match group {
                "source" | "integration" if row.deleted => {
                    forgotten.push((group == "integration", SourceId::new(name)));
                }
                "source" | "integration" => {
                    sources.push((group == "integration", SourceId::new(name), decode(row)?));
                }
                "preference" => {
                    let value = row.value.clone().ok_or("The preference is missing")?;
                    let name = name.to_owned();
                    owner
                        .settings_apply
                        .update_preferences(move |ui| set_preference(ui, &name, value))?;
                }
                "scrobbling" if !row.deleted => {
                    self.apply_scrobbling(&owner, name, decode(row)?).await?;
                }
                // The network starts again once a running join or action ends.
                "relay" if !row.deleted => {
                    let value: RelayValue = decode(row)?;
                    let owner = Arc::clone(&owner);
                    owner.runtime.clone().spawn(async move {
                        owner.relay_changed(value.relay, value.public_relay).await
                    });
                }
                _ => {}
            }
        }
        if !sources.is_empty() {
            self.apply_sources(&owner, sources).await?;
        }
        for (integration, id) in forgotten {
            if integration {
                owner.settings_apply.restore(
                    |stored| {
                        stored
                            .sources
                            .integrations
                            .retain(|source| source.configuration.source_id != id);
                        Ok(())
                    },
                    false,
                )?;
            } else if owner
                .settings
                .load()
                .sources
                .configured
                .iter()
                .any(|source| source.configuration.source_id == id)
            {
                owner.source.forget_source(id).await.map_err(error)?;
            }
        }
        Ok(())
    }

    /// Puts received source and integration configurations that differ from this device's
    /// into the settings. Another device's Local source becomes a peer source here.
    async fn apply_sources(
        &self,
        owner: &Arc<ConnectOwner>,
        sources: Vec<(bool, SourceId, SourceValue)>,
    ) -> Result<(), String> {
        let stored = owner.settings.load();
        let secrets = Arc::clone(&owner.secrets);
        let device = self.device.clone();
        let names = self.devices();
        let prepared = tokio::task::spawn_blocking(move || {
            sources
                .into_iter()
                .filter_map(|(integration, id, value)| {
                    let existing = stored
                        .sources
                        .connections()
                        .find(|source| source.configuration.source_id == id)
                        .cloned();
                    let received = received_source(
                        id,
                        value,
                        existing.clone(),
                        &device,
                        &names,
                        secrets.as_ref(),
                    );
                    received
                        .map(|source| source.filter(|source| Some(source) != existing.as_ref()))
                        .transpose()
                        .map(|source| source.map(|source| (integration, source)))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .await
        .map_err(error)??;
        // Only a source whose configuration changed sets the sources up again.
        if prepared.is_empty() {
            return Ok(());
        }
        let catalog = Arc::clone(&self.catalog);
        let settings = owner.settings_apply.clone();
        owner
            .source
            .restore_user_state(true, true, || async move {
                let changed = prepared.clone();
                tokio::task::spawn_blocking(move || {
                    settings.restore(
                        |stored| {
                            for (integration, source) in changed {
                                let id = &source.configuration.source_id;
                                let known = stored
                                    .sources
                                    .connections()
                                    .any(|item| &item.configuration.source_id == id);
                                if !known && source.configuration.kind == "peer" {
                                    stored.ui.set_download_quality(
                                        id.clone(),
                                        sources::DownloadQuality::Transcoded(
                                            library::AudioEncoding {
                                                codec: library::AudioCodec::Opus,
                                                bitrate_kbps: Some(128),
                                                channels: library::AudioChannels::Preserve,
                                            },
                                        ),
                                    );
                                }
                                let list = match integration {
                                    true => &mut stored.sources.integrations,
                                    false => &mut stored.sources.configured,
                                };
                                match list
                                    .iter_mut()
                                    .find(|item| &item.configuration.source_id == id)
                                {
                                    Some(item) => *item = source,
                                    None => list.push(source),
                                }
                            }
                            Ok(())
                        },
                        true,
                    )
                })
                .await
                .map_err(error)??;
                for (_, source) in prepared {
                    let configuration = &source.configuration;
                    catalog
                        .reconcile_source(
                            &configuration.source_id,
                            &configuration.kind,
                            &configuration.name,
                        )
                        .await
                        .map_err(error)?;
                }
                Ok(())
            })
            .await
    }

    async fn apply_scrobbling(
        &self,
        owner: &Arc<ConnectOwner>,
        service: &str,
        value: ScrobblingValue,
    ) -> Result<(), String> {
        let file = owner.settings.clone();
        let secrets = Arc::clone(&owner.secrets);
        let service = service.to_owned();
        let prepared = tokio::task::spawn_blocking(move || {
            let mut prepared = file.load();
            let mut settings = serde_json::to_value(read_scrobbling(&prepared, secrets.as_ref())?)
                .map_err(error)?;
            let mut account = value.account;
            let secret: BTreeMap<String, String> = value
                .secret
                .map(|secret| serde_json::from_str(&secret))
                .transpose()
                .map_err(error)?
                .unwrap_or_default();
            for descriptor in scrobbling::secret_descriptors()
                .iter()
                .filter(|descriptor| descriptor.kind().starts_with(&service))
            {
                account[secret_field(descriptor.kind())] =
                    json!(secret.get(descriptor.kind()).cloned().unwrap_or_default());
            }
            settings[&service] = account;
            let settings: scrobbling::Settings = serde_json::from_value(settings).map_err(error)?;
            store_scrobbling(&mut prepared, secrets.as_ref(), &settings)?;
            Ok::<_, String>(prepared)
        })
        .await
        .map_err(error)??;
        owner.settings_apply.restore(
            |stored| {
                stored.scrobbling = prepared.scrobbling;
                stored.scrobbling_credentials = prepared.scrobbling_credentials;
                stored.scrobbling_secrets_present = false;
                stored.ui.lastfm_api_key = prepared.ui.lastfm_api_key;
                Ok(())
            },
            true,
        )
    }

    /// Seals the secret of a setting value to the profile key as it leaves the device.
    fn seal(&self, value: &mut Value) -> Result<(), String> {
        if let Some(secret) = value.get("secret").and_then(Value::as_str) {
            let sealed = self.encrypt(secret.as_bytes())?;
            value["secret"] = json!(base64::engine::general_purpose::STANDARD.encode(sealed));
        }
        Ok(())
    }

    /// Opens a secret another device sealed to the profile key.
    fn open(&self, value: &mut Value) -> Result<(), String> {
        if let Some(sealed) = value.get("secret").and_then(Value::as_str) {
            let sealed = base64::engine::general_purpose::STANDARD
                .decode(sealed)
                .map_err(error)?;
            value["secret"] = json!(String::from_utf8(self.decrypt(&sealed)?).map_err(error)?);
        }
        Ok(())
    }

    /// The value of each live row that holds none, as it leaves this device.
    async fn values(&self, rows: &mut [Row]) -> Result<(), String> {
        let mut wanted = BTreeMap::<String, Vec<String>>::new();
        for row in rows
            .iter()
            .filter(|row| !row.deleted && row.value.is_none())
        {
            if let Some((kind, object)) = row.id.split_once(':') {
                wanted
                    .entry(kind.to_owned())
                    .or_default()
                    .push(object.to_owned());
            }
        }
        let mut values = HashMap::new();
        for (kind, objects) in wanted {
            for mut item in self.items(&kind, Some(&objects)).await? {
                if item.held {
                    continue;
                }
                if kind == "setting" {
                    self.seal(&mut item.value)?;
                }
                values.insert(item.id, item.value);
            }
        }
        for row in rows
            .iter_mut()
            .filter(|row| !row.deleted && row.value.is_none())
        {
            row.value = values.remove(&row.id);
        }
        Ok(())
    }
}

impl rufin_connect::sync::Values for Records {
    fn encode<'a>(&'a self, rows: &'a mut [Row]) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move { self.values(rows).await.map_err(anyhow::Error::msg) })
    }
}

/// A setting item whose digest covers its secret as a hash keyed with `key`.
fn secret_item(id: String, value: &impl Serialize, key: &[u8; 32]) -> Result<Item, String> {
    let value = serde_json::to_value(value).map_err(error)?;
    let mut digest = value.clone();
    if let Some(secret) = digest.get("secret").and_then(Value::as_str) {
        digest["secret"] = json!(blake3::keyed_hash(key, secret.as_bytes()).to_hex().as_str());
    }
    Item::digested(id, None, &value, &digest)
}

fn kind(id: &str) -> &str {
    id.split_once(':').map_or(id, |(kind, _)| kind)
}

fn object(row: &Row, prefix: &str) -> Result<String, String> {
    row.id
        .strip_prefix(prefix)
        .map(str::to_owned)
        .ok_or_else(|| format!("Invalid Connect record {}", row.id))
}

fn decode<T: DeserializeOwned>(row: &Row) -> Result<T, String> {
    serde_json::from_value(row.value.clone().ok_or("The record value is missing")?).map_err(error)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    (0..text.len())
        .step_by(2)
        .map(|at| {
            text.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| "Invalid hex text".to_owned())
        })
        .collect()
}

fn legacy_key(key: &str) -> Result<library::LegacyActivityKey, String> {
    let mut parts = key.splitn(4, '/').map(str::to_owned);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(source), Some(period), Some(kind), Some(object)) => {
            Ok((source, period, kind, object))
        }
        _ => Err(format!("Invalid activity key {key}")),
    }
}

fn source_value(
    source: &ConfiguredSource,
    device: &str,
    secrets: &dyn secrets::SecretStore,
) -> Result<SourceValue, String> {
    let configuration = &source.configuration;
    let owner = match configuration.kind.as_str() {
        "local" => Some(device.to_owned()),
        "peer" => {
            serde_json::from_str::<Value>(&configuration.provider_payload).map_err(error)?["device"]
                .as_str()
                .map(str::to_owned)
        }
        _ => None,
    };
    if owner.is_some() {
        return Ok(SourceValue {
            kind: "local".into(),
            name: None,
            payload: None,
            music_folder_id: source.music_folder_id.clone(),
            half_stars: source.enable_half_stars,
            owner,
            secret: None,
        });
    }
    Ok(SourceValue {
        kind: configuration.kind.clone(),
        name: Some(configuration.name.clone()),
        payload: Some(configuration.provider_payload.clone()),
        music_folder_id: source.music_folder_id.clone(),
        half_stars: source.enable_half_stars,
        owner: None,
        secret: source
            .credential
            .as_ref()
            .map(|credential| load_provider_secret(secrets, credential))
            .transpose()?
            .flatten(),
    })
}

/// The configuration a received source record makes here, or None when it changes nothing:
/// this device's own Local source takes only the shared fields.
fn received_source(
    id: SourceId,
    value: SourceValue,
    existing: Option<ConfiguredSource>,
    device: &str,
    names: &BTreeMap<String, String>,
    secrets: &dyn secrets::SecretStore,
) -> Result<Option<ConfiguredSource>, String> {
    if value.kind == "local" {
        let owner = value.owner.ok_or("A Local source record names no device")?;
        if owner == device {
            return Ok(existing.map(|mut source| {
                source.music_folder_id = value.music_folder_id;
                source.enable_half_stars = value.half_stars;
                source
            }));
        }
        let payload = json!({ "device": owner }).to_string();
        let mut source = existing.unwrap_or_else(|| ConfiguredSource {
            configuration: sources::SourceConfiguration {
                source_id: id,
                kind: "peer".into(),
                name: names.get(&owner).cloned().unwrap_or_else(|| "Rufin".into()),
                provider_payload: payload.clone(),
            },
            credential: None,
            music_folder_id: None,
            local_access: None,
            enable_half_stars: false,
        });
        source.configuration.provider_payload = payload;
        source.music_folder_id = value.music_folder_id;
        source.enable_half_stars = value.half_stars;
        return Ok(Some(source));
    }
    let old = existing
        .as_ref()
        .and_then(|source| source.credential.clone());
    let unchanged = old
        .as_ref()
        .map(|credential| load_provider_secret(secrets, credential))
        .transpose()?
        .flatten()
        == value.secret;
    let credential = match unchanged {
        true => old,
        false => value
            .secret
            .as_deref()
            .map(|secret| seal_credential(secrets, secret))
            .transpose()?,
    };
    Ok(Some(ConfiguredSource {
        configuration: sources::SourceConfiguration {
            source_id: id,
            kind: value.kind,
            name: value.name.unwrap_or_default(),
            provider_payload: value.payload.unwrap_or_default(),
        },
        credential,
        music_folder_id: value.music_folder_id,
        local_access: existing.and_then(|source| source.local_access),
        enable_half_stars: value.half_stars,
    }))
}

/// A digest of what the setting records read from the settings file, logins as they are
/// sealed there: a save that leaves it as it was, such as the volume, needs no settings
/// pass and no keyring reads.
pub(super) fn shared_settings(stored: &crate::settings::StoredSettings) -> Option<String> {
    let preferences = PREFERENCES
        .iter()
        .map(|name| preference(&stored.ui, name))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    store::digest(&(
        &stored.sources.configured,
        &stored.sources.integrations,
        preferences,
        &stored.scrobbling,
        &stored.scrobbling_credentials,
        &stored.ui.lastfm_api_key,
    ))
    .ok()
}

fn preference(ui: &crate::settings::Settings, name: &str) -> Result<Value, String> {
    Ok(match name {
        "prefer_distinct_track_covers" => json!(ui.application.prefer_distinct_track_covers),
        "use_sort_tags" => json!(ui.application.use_sort_tags),
        "external_metadata_enabled" => json!(ui.application.external_metadata_enabled),
        "sidebar.pins" => serde_json::to_value(&ui.sidebar.pins).map_err(error)?,
        "lyrics.suppressed_auto_lyrics" => json!(ui.lyrics.suppressed_auto_lyrics_track_ids),
        _ => Value::Null,
    })
}

fn set_preference(
    ui: &mut crate::settings::Settings,
    name: &str,
    value: Value,
) -> Result<(), String> {
    fn read<T: DeserializeOwned>(value: Value) -> Result<T, String> {
        serde_json::from_value(value).map_err(error)
    }
    match name {
        "prefer_distinct_track_covers" => {
            ui.application.prefer_distinct_track_covers = read(value)?
        }
        "use_sort_tags" => ui.application.use_sort_tags = read(value)?,
        "external_metadata_enabled" => ui.application.external_metadata_enabled = read(value)?,
        "sidebar.pins" => ui.sidebar.pins = read(value)?,
        "lyrics.suppressed_auto_lyrics" => {
            ui.lyrics.suppressed_auto_lyrics_track_ids = read(value)?
        }
        _ => {}
    }
    Ok(())
}

/// A service's scrobbling account with its secrets moved into `secret`.
fn scrobbling_value(settings: &Value, service: &str) -> Result<ScrobblingValue, String> {
    let mut account = settings[service].clone();
    let mut secret = BTreeMap::new();
    for descriptor in scrobbling::secret_descriptors()
        .iter()
        .filter(|descriptor| descriptor.kind().starts_with(service))
    {
        let field = secret_field(descriptor.kind());
        if let Some(value) = account[field].as_str().filter(|value| !value.is_empty()) {
            secret.insert(descriptor.kind().to_owned(), value.to_owned());
        }
        account[field] = json!("");
    }
    Ok(ScrobblingValue {
        account,
        secret: (!secret.is_empty())
            .then(|| serde_json::to_string(&secret))
            .transpose()
            .map_err(error)?,
    })
}

/// The field of a scrobbling account a secret kind fills.
fn secret_field(kind: &str) -> &'static str {
    match kind {
        "lastfm-api-secret" => "api_secret",
        "lastfm-session" | "librefm-session" => "session_key",
        _ => "user_token",
    }
}
