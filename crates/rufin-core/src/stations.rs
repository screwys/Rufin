//! Radio stations. A Subsonic server keeps its own, and every change goes to it; a source that
//! keeps none shows the ones Rufin keeps. `radio-stations/` is Rufin's list: it gives a station
//! its description, genres, picture and channels, and suggests the ones not added yet.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use library::{
    QueueContextTitle, QueueProvenance, SidebarPin, StationRow, StationWrite, TrackReference,
};
use tracing::warn;

use crate::playback::{PlayRequest, PlaybackOwner, QueuePlacement};
use crate::source::SourceOwner;
use crate::string_error;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../radio-stations/"]
struct Files;

/// A station of Rufin's list, as its file states it.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct Listed {
    #[serde(skip)]
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    logo: Option<String>,
    channels: Vec<Channel>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct Channel {
    name: String,
    stream: String,
}

/// Rufin's list, by name. A file that does not read is left out.
fn listed() -> &'static [Listed] {
    static LIST: OnceLock<Vec<Listed>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut list = Files::iter()
            .filter_map(|path| {
                let id = path.strip_suffix(".json")?.to_string();
                match serde_json::from_slice::<Listed>(&Files::get(&path)?.data) {
                    Ok(station)
                        if !station.name.trim().is_empty() && !station.channels.is_empty() =>
                    {
                        Some(Listed { id, ..station })
                    }
                    Ok(_) => {
                        warn!(station = %id, "a listed radio station has no name or channel");
                        None
                    }
                    Err(error) => {
                        warn!(station = %id, %error, "could not read a listed radio station");
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        list.sort_by_key(|station| station.name.to_lowercase());
        list
    })
}

fn same_stream(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// The listed station and channel a stream is.
fn listing(stream: &str) -> Option<(&'static Listed, &'static Channel)> {
    listed().iter().find_map(|station| {
        station
            .channels
            .iter()
            .find(|channel| same_stream(&channel.stream, stream))
            .map(|channel| (station, channel))
    })
}

/// A channel of a station with several: a click plays it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationChannel {
    pub name: String,
    pub playing: bool,
    /// The station's own stream is this channel's: a click on its card plays it, and Edit
    /// chooses it.
    pub own: bool,
    /// The channel's own sidebar pin, where Saved is shown and the station is added.
    pub pin: Option<SidebarPin>,
    pub pinned: bool,
}

/// A station as its card shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Station {
    /// Its Store row; None for a suggested station that was not added.
    pub key: Option<i64>,
    /// Its id in Rufin's list, when the list has it.
    pub listed: Option<String>,
    pub name: String,
    pub description: String,
    pub genres: Vec<String>,
    pub homepage: String,
    pub stream_url: String,
    /// The channels Rufin's list gives it; empty for a station with one stream.
    pub channels: Vec<StationChannel>,
    pub artwork_binding: Option<Vec<u8>>,
    /// A person chose its picture; Edit can take that one away.
    pub picture_chosen: bool,
    pub playing: bool,
    /// Its sidebar pin, where Saved is shown; None for a suggested station.
    pub pin: Option<SidebarPin>,
    pub pinned: bool,
    /// The channel a pinned station stands for in Saved, which a click on it plays.
    pub channel: Option<String>,
}

impl Station {
    /// The item the station plays `stream` as: its name for a title and its genres for an
    /// artist.
    fn item(&self, stream: &str) -> TrackReference {
        let mut item = TrackReference {
            media_uri: library::station_media_uri(stream),
            artwork_binding: self.artwork_binding.clone(),
            ..Default::default()
        };
        item.metadata.title = self.name.clone();
        item.metadata.artist = self.genres.join(", ");
        item
    }

    /// The station as a row, for a list that holds library rows: the item it plays as, which
    /// is no track of the library.
    pub fn row(&self) -> library::Row {
        let item = self.item(&self.stream_url);
        library::Row::Track(
            library::TrackView {
                track_key: library::TrackKey::from_raw(0),
                source_key: None,
                source_id: None,
                object_id: None,
                media_uri: item.media_uri,
                metadata: item.metadata,
                state: Default::default(),
                access: Default::default(),
                links: Default::default(),
                artwork_binding: item.artwork_binding,
            }
            .into(),
        )
    }
}

/// A song a station announced since it was tuned in.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HeardSong {
    pub at_unix_seconds: i64,
    pub title: String,
    pub artist: String,
}

/// The station the page's header shows: the one that plays, or with none in the queue, the
/// page's first station, ready to play.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct OnAir {
    pub station: Station,
    /// The station is the queue's item, so Play and Pause are the player's. Otherwise Play
    /// plays the station.
    pub tuned: bool,
    /// The channel that plays, of a station with several.
    pub channel: Option<String>,
    /// The song the stream announces.
    pub title: Option<String>,
    pub artist: Option<String>,
    /// How the stream is encoded, such as "MP3 · 192 kbps".
    pub format: Option<String>,
    /// Newest first.
    pub heard: Vec<HeardSong>,
    /// It plays or is about to; false while paused.
    pub playing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RadioPage {
    pub on_air: Option<OnAir>,
    pub stations: Vec<Station>,
    /// Rufin's list, without the stations already added.
    pub suggested: Vec<Station>,
    /// The server a station is saved on; None when Rufin keeps it.
    pub saved_on: Option<String>,
}

/// What a person entered for a station.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationInput {
    pub name: String,
    pub stream_url: String,
    pub homepage: String,
    /// The address of its picture, where the search that found it gave one; Edit leaves it.
    pub logo: String,
}

/// What a stream says of itself, for adding it by its address.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationProbe {
    /// The stream itself, where the address named a playlist.
    pub stream_url: String,
    pub name: String,
    pub genre: String,
    pub format: String,
    pub host: String,
}

/// A station radio-browser.info found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StationFound {
    pub name: String,
    pub stream_url: String,
    pub homepage: String,
    /// Genre, format and host on one line.
    pub detail: String,
    /// The address of its picture; adding the station keeps it.
    pub logo: String,
    pub artwork_binding: Option<Vec<u8>>,
    /// The page's stations hold this stream already.
    pub added: bool,
}

/// Where a station is kept: a server, or Rufin. The selected source says where a new one goes.
enum Keeper {
    Server {
        key: library::SourceKey,
        id: sources::SourceId,
        name: String,
    },
    Rufin,
}

impl Keeper {
    fn of(owner: &SourceOwner) -> Self {
        let Some(selected) = owner.selected_library() else {
            return Self::Rufin;
        };
        match owner.configuration(&selected.source_id) {
            Some(configuration)
                if matches!(configuration.kind.as_str(), "navidrome" | "subsonic") =>
            {
                Self::Server {
                    key: selected.source_key,
                    id: selected.source_id,
                    name: configuration.name,
                }
            }
            _ => Self::Rufin,
        }
    }

    /// The server that keeps a station of `key`.
    fn keeping(owner: &SourceOwner, key: library::SourceKey) -> Option<Self> {
        let id = owner.shared.catalog.source_id(key)?;
        let name = owner.configuration(&id)?.name;
        Some(Self::Server { key, id, name })
    }

    fn source_key(&self) -> Option<library::SourceKey> {
        match self {
            Self::Server { key, .. } => Some(*key),
            Self::Rufin => None,
        }
    }

    async fn provider(&self, owner: &SourceOwner) -> Result<Option<Arc<sources::Source>>, String> {
        let Self::Server { id, .. } = self else {
            return Ok(None);
        };
        let (owner, id) = (owner.clone(), id.clone());
        tokio::task::spawn_blocking(move || owner.client(&id))
            .await
            .map_err(string_error)?
            .map(Some)
    }
}

/// The stream that plays when the current item is a radio station.
fn playing_stream(playback: &PlaybackOwner) -> Option<(String, Arc<playback::CurrentMedia>)> {
    let media = playback.current_media()?;
    Some((library::station_stream(&media.id.media_uri)?, media))
}

fn station_of(
    owner: &SourceOwner,
    row: Option<&StationRow>,
    stream_url: &str,
    playing: Option<&str>,
) -> Station {
    let catalog = &owner.shared.catalog;
    let stored = owner.shared.settings.load();
    let sidebar = &stored.ui.sidebar;
    let pin = row
        .and_then(|row| catalog.station_pin(row))
        .filter(|_| sidebar.pins_visible);
    let source = row
        .and_then(|row| row.source_key)
        .and_then(|key| catalog.source_id(key));
    let listing = listing(stream_url);
    let list = listing.map(|(station, _)| station);
    let own = row.map_or(stream_url, |row| row.stream_url.as_str());
    let channels = list
        .filter(|station| station.channels.len() > 1)
        .map(|station| {
            station
                .channels
                .iter()
                .map(|channel| {
                    let pin = pin.clone().map(|pin| match pin {
                        SidebarPin::Station {
                            source_id,
                            station_id,
                            ..
                        } => SidebarPin::Station {
                            source_id,
                            station_id,
                            channel: Some(channel.name.clone()),
                        },
                        pin => pin,
                    });
                    StationChannel {
                        name: channel.name.clone(),
                        playing: playing.is_some_and(|stream| same_stream(&channel.stream, stream)),
                        own: same_stream(&channel.stream, own),
                        pinned: pin.as_ref().is_some_and(|pin| sidebar.is_pinned(pin)),
                        pin,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let native = row.and_then(|row| Some((source.as_ref()?, row.cover.as_deref()?)));
    Station {
        key: row.map(|row| row.station_key),
        listed: list.map(|station| station.id.clone()),
        name: row
            .map(|row| row.name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| list.map(|station| station.name.clone()))
            .unwrap_or_else(|| stream_url.to_string()),
        description: list
            .map(|station| station.description.clone())
            .unwrap_or_default(),
        genres: list
            .map(|station| station.genres.clone())
            .unwrap_or_default(),
        homepage: row
            .map(|row| row.homepage.clone())
            .filter(|homepage| !homepage.is_empty())
            .or_else(|| list.map(|station| station.homepage.clone()))
            .unwrap_or_default(),
        stream_url: stream_url.to_string(),
        playing: playing.is_some_and(|stream| {
            same_stream(stream_url, stream)
                || list.is_some_and(|station| {
                    station
                        .channels
                        .iter()
                        .any(|channel| same_stream(&channel.stream, stream))
                })
        }),
        channels,
        artwork_binding: library::station_artwork(
            row.and_then(|row| Some((row.station_key, row.picture.as_deref()?))),
            native,
            row.and_then(|row| row.logo.as_deref())
                .or(list.and_then(|station| station.logo.as_deref())),
        ),
        picture_chosen: row.is_some_and(|row| row.picture.is_some()),
        pinned: pin.as_ref().is_some_and(|pin| sidebar.is_pinned(pin)),
        pin,
        channel: None,
    }
}

/// A pinned station as Saved shows it. A pinned channel is named after the station and
/// itself, and plays its own stream.
pub(crate) fn pinned(owner: &SourceOwner, pin: &SidebarPin, row: &StationRow) -> Station {
    let station = station_of(owner, Some(row), &row.stream_url, None);
    let SidebarPin::Station {
        channel: Some(channel),
        ..
    } = pin
    else {
        return station;
    };
    let stream = listing(&row.stream_url)
        .and_then(|(station, _)| {
            station
                .channels
                .iter()
                .find(|listed| &listed.name == channel)
        })
        .map_or_else(|| row.stream_url.clone(), |listed| listed.stream.clone());
    Station {
        name: format!("{} {channel}", station.name),
        stream_url: stream,
        channel: Some(channel.clone()),
        channels: Vec::new(),
        pin: Some(pin.clone()),
        pinned: true,
        ..station
    }
}

/// How a stream is encoded, as the page's facts show it.
pub(crate) fn format(live: &playback::LiveStream, content_type: Option<&str>) -> Option<String> {
    let codec = live.codec.as_deref().unwrap_or_default();
    let named = ["MP3", "AAC", "Opus", "Vorbis", "FLAC"]
        .into_iter()
        .find(|name| {
            codec
                .to_ascii_lowercase()
                .contains(&name.to_ascii_lowercase())
        })
        .or(match content_type.unwrap_or_default() {
            "audio/mpeg" => Some("MP3"),
            "audio/aac" | "audio/aacp" => Some("AAC"),
            "audio/flac" => Some("FLAC"),
            _ => None,
        });
    let bitrate = live
        .bitrate
        .map(|bitrate| format!("{} kbps", (f64::from(bitrate) / 1000.0).round()));
    match (named, bitrate) {
        (Some(name), Some(bitrate)) => Some(format!("{name} · {bitrate}")),
        (Some(name), None) => Some(name.into()),
        (None, bitrate) => bitrate,
    }
}

/// The Radio page: the station on air, every source's stations and the suggested ones.
pub async fn page(owner: &SourceOwner, playback: &PlaybackOwner) -> Result<RadioPage, String> {
    let keeper = Keeper::of(owner);
    let rows = owner
        .shared
        .catalog
        .stations()
        .await
        .map_err(string_error)?;
    let playing = playing_stream(playback);
    let stream = playing.as_ref().map(|(stream, _)| stream.as_str());
    let mut stations = rows
        .iter()
        .map(|row| station_of(owner, Some(row), &row.stream_url, stream))
        .collect::<Vec<_>>();
    // A source that lists a station's channels as entries of their own has them as they are:
    // each a card without the other channels, playing when its own stream does.
    let separate = stations
        .iter()
        .zip(&rows)
        .map(|(station, row)| {
            station.listed.is_some()
                && stations
                    .iter()
                    .zip(&rows)
                    .filter(|(other, placed)| {
                        other.listed == station.listed && placed.source_key == row.source_key
                    })
                    .count()
                    > 1
        })
        .collect::<Vec<_>>();
    for (station, _) in stations
        .iter_mut()
        .zip(separate)
        .filter(|(_, separate)| *separate)
    {
        station.channels.clear();
        station.playing = stream.is_some_and(|stream| same_stream(&station.stream_url, stream));
    }
    let suggested = listed()
        .iter()
        .filter(|station| {
            !stations
                .iter()
                .any(|added| added.listed.as_ref() == Some(&station.id))
        })
        .map(|station| station_of(owner, None, &station.channels[0].stream, stream))
        .collect::<Vec<_>>();
    let on_air = playing.map(|(stream, media)| {
        let station = stations
            .iter()
            .chain(&suggested)
            .find(|station| station.playing)
            .cloned()
            .unwrap_or_else(|| Station {
                name: media
                    .live
                    .as_ref()
                    .map(|live| live.station.clone())
                    .unwrap_or_default(),
                playing: true,
                ..station_of(owner, None, &stream, Some(&stream))
            });
        let live = media.live.as_ref();
        let announced = live.is_some_and(|live| live.station != media.row.track.metadata.title);
        let metadata = &media.row.track.metadata;
        OnAir {
            channel: station
                .channels
                .iter()
                .find(|channel| channel.playing)
                .map(|channel| channel.name.clone()),
            title: announced.then(|| metadata.title.clone()),
            artist: announced
                .then(|| metadata.artist.clone())
                .filter(|artist| !artist.is_empty()),
            format: live.and_then(|live| {
                format(
                    live,
                    media
                        .stream
                        .as_ref()
                        .and_then(|stream| stream.format.as_deref()),
                )
            }),
            heard: playback.heard(),
            playing: playback
                .updates
                .current()
                .is_some_and(|view| view.transport.desired_playing),
            tuned: true,
            station,
        }
    });
    let on_air = on_air.or_else(|| {
        Some(OnAir {
            station: stations.first().or(suggested.first())?.clone(),
            tuned: false,
            channel: None,
            title: None,
            artist: None,
            format: None,
            heard: Vec::new(),
            playing: false,
        })
    });
    Ok(RadioPage {
        on_air,
        stations,
        suggested,
        saved_on: match keeper {
            Keeper::Server { name, .. } => Some(name),
            Keeper::Rufin => None,
        },
    })
}

/// Reads the stations of every server that keeps some into the Store. One that does not
/// answer keeps its copy, and its error is the one answered.
pub async fn refresh(owner: &SourceOwner) -> Result<(), String> {
    let catalog = &owner.shared.catalog;
    let servers = owner
        .shared
        .settings
        .load()
        .sources
        .configured
        .iter()
        .filter(|source| matches!(source.configuration.kind.as_str(), "navidrome" | "subsonic"))
        .filter_map(|source| catalog.source_key(&source.configuration.source_id))
        .filter_map(|key| Keeper::keeping(owner, key))
        .collect::<Vec<_>>();
    let mut failed = Ok(());
    for keeper in servers {
        let read = async {
            let (Some(source), Some(key)) = (keeper.provider(owner).await?, keeper.source_key())
            else {
                return Ok(());
            };
            let listed = source.stations().await.map_err(string_error)?;
            catalog
                .replace_stations(key, &listed)
                .await
                .map_err(string_error)
        }
        .await;
        if read.is_err() {
            failed = read;
        }
    }
    failed
}

fn random_id() -> String {
    format!(
        "{:016x}{:016x}",
        crate::playback::random_u64(),
        crate::playback::random_u64()
    )
}

/// Adds a station: on the selected source's server when it keeps stations, else in Rufin.
pub async fn add(owner: &SourceOwner, input: StationInput) -> Result<(), String> {
    let (name, stream_url) = (input.name.trim(), input.stream_url.trim());
    if name.is_empty() || stream_url.is_empty() {
        return Err("A station needs a name and a stream address".into());
    }
    let keeper = Keeper::of(owner);
    let written = match keeper.provider(owner).await? {
        Some(source) => source
            .create_station(name, stream_url, &input.homepage)
            .await
            .map_err(string_error)?,
        None => StationWrite {
            object_id: random_id(),
            name: name.into(),
            stream_url: stream_url.into(),
            homepage: input.homepage.trim().into(),
            cover: None,
        },
    };
    let catalog = &owner.shared.catalog;
    let row = catalog
        .put_station(keeper.source_key(), &written)
        .await
        .map_err(string_error)?;
    let logo = input.logo.trim();
    if logo.is_empty() {
        return Ok(());
    }
    catalog
        .set_station_logo(row.station_key, Some(logo))
        .await
        .map_err(string_error)
}

/// Adds a station of Rufin's list, with every channel shown.
pub async fn add_listed(owner: &SourceOwner, id: String) -> Result<(), String> {
    let station = listed()
        .iter()
        .find(|station| station.id == id)
        .ok_or("Rufin's list has no such station")?;
    add(
        owner,
        StationInput {
            name: station.name.clone(),
            stream_url: station.channels[0].stream.clone(),
            homepage: station.homepage.clone(),
            logo: String::new(),
        },
    )
    .await
}

/// The pictures Edit shows for a station: the one chosen for it, and the one it has without.
pub async fn picture_editing(
    owner: &SourceOwner,
    key: i64,
) -> Result<crate::playlists::PlaylistArtworkEditing, String> {
    let row = owner
        .shared
        .catalog
        .station(key)
        .await
        .map_err(string_error)?
        .ok_or("The station is gone")?;
    let station = station_of(owner, Some(&row), &row.stream_url, None);
    let unchosen = StationRow {
        picture: None,
        ..row
    };
    Ok(crate::playlists::PlaylistArtworkEditing {
        binding: station
            .picture_chosen
            .then_some(station.artwork_binding)
            .flatten(),
        automatic_binding: station_of(owner, Some(&unchosen), &unchosen.stream_url, None)
            .artwork_binding,
        writable: true,
    })
}

/// Changes a station. `channel` names the one its own stream becomes, of a station with
/// several: the one a click on its card plays. `picture` sets or takes away the picture
/// chosen for it, which Rufin keeps.
pub async fn edit(
    owner: &SourceOwner,
    key: i64,
    input: StationInput,
    channel: Option<String>,
    picture: Option<sources::ArtworkChange>,
) -> Result<(), String> {
    let catalog = &owner.shared.catalog;
    let row = catalog
        .station(key)
        .await
        .map_err(string_error)?
        .ok_or("The station is gone")?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err("A station needs a name".into());
    }
    let list = listing(&row.stream_url).map(|(station, _)| station);
    let stream_url = match (&channel, list) {
        (Some(channel), Some(station)) => station
            .channels
            .iter()
            .find(|listed| &listed.name == channel)
            .map_or_else(|| row.stream_url.clone(), |listed| listed.stream.clone()),
        _ if list.is_some() || input.stream_url.trim().is_empty() => row.stream_url.clone(),
        _ => input.stream_url.trim().to_string(),
    };
    let written = StationWrite {
        object_id: row.object_id.clone(),
        name: name.into(),
        stream_url,
        homepage: input.homepage.trim().into(),
        cover: row.cover.clone(),
    };
    if let Some(key) = row.source_key {
        let source = Keeper::keeping(owner, key)
            .ok_or("Source access is unavailable")?
            .provider(owner)
            .await?
            .ok_or("Source access is unavailable")?;
        source
            .update_station(
                &row.object_id,
                &written.name,
                &written.stream_url,
                &written.homepage,
            )
            .await
            .map_err(string_error)?;
    }
    catalog
        .put_station(row.source_key, &written)
        .await
        .map_err(string_error)?;
    match picture {
        Some(sources::ArtworkChange::Replace(image)) => catalog
            .set_station_picture(key, Some((&image.bytes, image.content_type.as_deref())))
            .await
            .map_err(string_error),
        Some(sources::ArtworkChange::Remove) => catalog
            .set_station_picture(key, None)
            .await
            .map_err(string_error),
        None => Ok(()),
    }
}

/// Orders the page's stations as `keys` lists them. Rufin keeps the order, also for a
/// server's stations.
pub async fn reorder(owner: &SourceOwner, keys: Vec<i64>) -> Result<(), String> {
    owner
        .shared
        .catalog
        .order_stations(&keys)
        .await
        .map_err(string_error)
}

pub async fn delete(owner: &SourceOwner, key: i64) -> Result<(), String> {
    let catalog = &owner.shared.catalog;
    let Some(row) = catalog.station(key).await.map_err(string_error)? else {
        return Ok(());
    };
    if let Some(key) = row.source_key {
        let source = Keeper::keeping(owner, key)
            .ok_or("Source access is unavailable")?
            .provider(owner)
            .await?
            .ok_or("Source access is unavailable")?;
        source
            .delete_station(&row.object_id)
            .await
            .map_err(string_error)?;
    }
    catalog.delete_station(key).await.map_err(string_error)?;
    // Its pin goes with it, and those of its channels.
    if let Some(SidebarPin::Station {
        source_id,
        station_id,
        ..
    }) = catalog.station_pin(&row)
    {
        let settings = owner.shared.settings.clone();
        let unpinned = tokio::task::spawn_blocking(move || {
            settings.update(|stored| {
                let pins = &mut stored.ui.sidebar.pins;
                let before = pins.len();
                pins.retain(|pin| {
                    !matches!(pin, SidebarPin::Station { source_id: source, station_id: station, .. }
                        if *source == source_id && *station == station_id)
                });
                Ok(pins.len() != before)
            })
        })
        .await
        .map_err(string_error)
        .and_then(|result| result);
        if let Err(error) = unpinned {
            owner.shared.warn_nonfatal(&error);
        }
    }
    Ok(())
}

/// Plays a station, alone in the queue: an added one by its key, a suggested one by its list
/// id. `channel` names the channel of a station with several; without it the first shown plays.
pub async fn play(
    owner: &SourceOwner,
    playback: &PlaybackOwner,
    key: Option<i64>,
    listed_id: Option<String>,
    channel: Option<String>,
) -> Result<(), String> {
    let row = match key {
        Some(key) => owner
            .shared
            .catalog
            .station(key)
            .await
            .map_err(string_error)?,
        None => None,
    };
    let list = match &row {
        Some(row) => listing(&row.stream_url).map(|(station, _)| station),
        None => listed()
            .iter()
            .find(|station| Some(&station.id) == listed_id.as_ref()),
    };
    // An added station plays its own stream: a server may list each channel as an entry of
    // its own. A named channel, and a suggested station's first, come from Rufin's list.
    let own = row
        .as_ref()
        .filter(|_| channel.is_none())
        .map(|row| row.stream_url.clone());
    let stream = own
        .or_else(|| {
            list.and_then(|station| {
                station
                    .channels
                    .iter()
                    .find(|listed| {
                        channel
                            .as_ref()
                            .is_none_or(|channel| &listed.name == channel)
                    })
                    .map(|channel| channel.stream.clone())
            })
        })
        .or_else(|| row.as_ref().map(|row| row.stream_url.clone()))
        .ok_or("The station is gone")?;
    let station = station_of(owner, row.as_ref(), &stream, None);
    let item = station.item(&stream);
    let provenance = QueueProvenance::Context {
        context_id: format!("station:{stream}").into(),
        context_title: Some(Arc::new(QueueContextTitle {
            kind: None,
            title: station.name.into(),
        })),
        source_rank: 0,
    };
    playback.play_selection(PlayRequest::from_input(
        library::QueueLoad::items(vec![(item, provenance)]),
        QueuePlacement::Replace { anchor_index: 0 },
    ));
    Ok(())
}

fn client() -> Result<reqwest::Client, String> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("Rufin/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(string_error)?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// The first stream a `.pls` or `.m3u` playlist names; None for an HLS playlist, which the
/// player reads itself.
fn playlist_stream(text: &str) -> Option<String> {
    if text.contains("#EXT-X-") {
        return None;
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with('['))
        .filter_map(|line| match line.split_once('=') {
            Some((key, value)) if key.trim().to_ascii_lowercase().starts_with("file") => {
                Some(value.trim())
            }
            Some(_) if !line.contains("://") => None,
            _ => Some(line),
        })
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(str::to_string)
}

/// Opens a station's address and answers the stream it leads to, with the answer's headers.
/// An address that names a playlist leads to the first stream the playlist names.
async fn open_stream(address: &str) -> Result<(String, reqwest::header::HeaderMap), String> {
    let mut address = address.trim().to_string();
    for _ in 0..3 {
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            client()?.get(&address).header("Icy-MetaData", "1").send(),
        )
        .await
        .map_err(|_| "The station did not answer".to_string())?
        .map_err(|error| error.without_url().to_string())?
        .error_for_status()
        .map_err(|error| error.without_url().to_string())?;
        let kind = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let path = response.url().path().to_ascii_lowercase();
        let playlist = ["scpls", "pls+xml", "mpegurl"]
            .iter()
            .any(|name| kind.contains(name))
            || (!kind.starts_with("audio/")
                && [".pls", ".m3u", ".m3u8"]
                    .iter()
                    .any(|end| path.ends_with(end)));
        if !playlist {
            return Ok((response.url().to_string(), response.headers().clone()));
        }
        let (url, headers) = (response.url().to_string(), response.headers().clone());
        let mut body = Vec::new();
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| error.without_url().to_string())?
        {
            body.extend_from_slice(&chunk);
            if body.len() > 256 * 1024 {
                break;
            }
        }
        match playlist_stream(&String::from_utf8_lossy(&body)) {
            Some(next) => address = next,
            None => return Ok((url, headers)),
        }
    }
    Err("The station's address leads to too many playlists".into())
}

/// The stream a station's address plays.
pub(crate) async fn resolve_stream(address: &str) -> Result<String, String> {
    open_stream(address).await.map(|(stream, _)| stream)
}

/// Reads what a stream says of itself, for adding a station by its address.
pub async fn probe(address: String) -> Result<StationProbe, String> {
    let address = address.trim();
    let parsed = url::Url::parse(address).map_err(|_| "Enter the station's stream address")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Enter the station's stream address".into());
    }
    let (stream_url, headers) = open_stream(address).await?;
    let header = |name: &str| {
        headers
            .get(name)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).trim().to_string())
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("n/a"))
    };
    let kind = header("content-type").unwrap_or_default();
    let codec = match kind.split(';').next().unwrap_or_default().trim() {
        "audio/mpeg" => "MP3",
        "audio/aac" | "audio/aacp" => "AAC",
        "audio/ogg" | "application/ogg" => "Ogg",
        "audio/flac" => "FLAC",
        _ => "",
    };
    let format = [
        codec.to_string(),
        header("icy-br")
            .map(|rate| format!("{rate} kbps"))
            .unwrap_or_default(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    let host = url::Url::parse(&stream_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    Ok(StationProbe {
        name: header("icy-name").unwrap_or_else(|| host.clone()),
        genre: header("icy-genre").unwrap_or_default(),
        stream_url,
        format,
        host,
    })
}

/// Finds stations by name on radio-browser.info, most listened first.
pub async fn search(owner: &SourceOwner, query: String) -> Result<Vec<StationFound>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if owner.shared.settings.load().ui.application.private_mode {
        return Err("Private mode is on, so Rufin does not search radio-browser.info".into());
    }
    let url = url::Url::parse_with_params(
        "https://all.api.radio-browser.info/json/stations/search",
        [
            ("name", query),
            ("order", "clickcount"),
            ("reverse", "true"),
            ("hidebroken", "true"),
            ("limit", "20"),
        ],
    )
    .map_err(string_error)?;
    let found: Vec<serde_json::Value> = tokio::time::timeout(Duration::from_secs(15), async {
        client()?
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| error.without_url().to_string())?
            .json()
            .await
            .map_err(|error| error.without_url().to_string())
    })
    .await
    .map_err(|_| "radio-browser.info did not answer".to_string())??;
    let rows = owner
        .shared
        .catalog
        .stations()
        .await
        .map_err(string_error)?;
    let text = |value: &serde_json::Value, key: &str| {
        value[key].as_str().unwrap_or_default().trim().to_string()
    };
    Ok(found
        .iter()
        .filter_map(|station| {
            let stream_url = Some(text(station, "url_resolved"))
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| text(station, "url"));
            let name = text(station, "name");
            if name.is_empty() || !stream_url.starts_with("http") {
                return None;
            }
            let host = url::Url::parse(&stream_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_default();
            let tags = text(station, "tags")
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join(", ");
            let bitrate = station["bitrate"].as_u64().filter(|rate| *rate > 0);
            let format = [
                text(station, "codec"),
                bitrate.map(|rate| rate.to_string()).unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
            let logo = Some(text(station, "favicon"))
                .filter(|url| url.starts_with("https://"))
                .unwrap_or_default();
            Some(StationFound {
                detail: [tags, format, host]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
                artwork_binding: library::station_artwork(None, None, Some(logo.as_str())),
                logo,
                added: rows
                    .iter()
                    .any(|row| same_stream(&row.stream_url, &stream_url)),
                homepage: text(station, "homepage"),
                name,
                stream_url,
            })
        })
        .collect())
}

/// The songs the playing station announced since it was tuned in.
#[derive(Default)]
pub(crate) struct Heard {
    station: String,
    songs: Vec<HeardSong>,
}

impl Heard {
    const KEPT: usize = 50;

    /// Notes the current item: a station's newly announced song joins the list, and another
    /// station or a track starts it again.
    pub(crate) fn note(heard: &Mutex<Self>, media: Option<&playback::CurrentMedia>) {
        let mut heard = heard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((media, live)) = media.and_then(|media| Some((media, media.live.as_ref()?)))
        else {
            *heard = Self::default();
            return;
        };
        if heard.station != media.id.media_uri {
            *heard = Self {
                station: media.id.media_uri.clone(),
                songs: Vec::new(),
            };
        }
        let metadata = &media.row.track.metadata;
        if live.station == metadata.title
            || heard
                .songs
                .first()
                .is_some_and(|song| song.title == metadata.title && song.artist == metadata.artist)
        {
            return;
        }
        heard.songs.insert(
            0,
            HeardSong {
                at_unix_seconds: chrono::Utc::now().timestamp(),
                title: metadata.title.clone(),
                artist: metadata.artist.clone(),
            },
        );
        heard.songs.truncate(Self::KEPT);
    }

    pub(crate) fn songs(heard: &Mutex<Self>) -> Vec<HeardSong> {
        heard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .songs
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_station_reads_and_names_secure_streams_once() {
        let files = Files::iter().filter(|path| path.ends_with(".json")).count();
        assert_eq!(
            listed().len(),
            files,
            "a file in radio-stations/ does not read"
        );
        let mut streams = Vec::new();
        for station in listed() {
            for channel in &station.channels {
                assert!(!channel.name.trim().is_empty(), "{}", station.id);
                assert!(channel.stream.starts_with("https://"), "{}", channel.stream);
                assert!(
                    !streams.contains(&channel.stream),
                    "{} is listed twice",
                    channel.stream
                );
                streams.push(channel.stream.clone());
            }
        }
    }

    #[test]
    fn a_playlist_names_its_first_stream_and_hls_is_left_to_the_player() {
        let pls = "[playlist]\nNumberOfEntries=2\nFile1=https://ice.example/one\nTitle1=One\nFile2=https://ice.example/two\n";
        assert_eq!(
            playlist_stream(pls).as_deref(),
            Some("https://ice.example/one")
        );
        let m3u = "#EXTM3U\n#EXTINF:-1,Station\nhttps://ice.example/live.mp3\n";
        assert_eq!(
            playlist_stream(m3u).as_deref(),
            Some("https://ice.example/live.mp3")
        );
        assert_eq!(
            playlist_stream("#EXTM3U\n#EXT-X-VERSION:3\nchunk.ts\n"),
            None
        );
    }
}
