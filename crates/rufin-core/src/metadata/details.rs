use super::MetadataItemId;
use crate::playback::PlaybackOwner;
use crate::route::{
    Route,
    detail_links::{DetailLinks, album_artist_links, metadata_links},
};
use crate::runtime::SourceHandle;
use crate::settings::LibraryField;
use crate::settings::presentation::{
    self, display_unix_date, format_duration_units, stored_rating,
};
use library::{LibraryError, LibraryResult, ReadCancellation, TrackState};
use localization::{msgid, tr};
use std::sync::Arc;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MediaDetailsItem {
    Track(String),
    Album(String),
    Artist {
        media_uri: String,
        album_artist: bool,
    },
}

impl MediaDetailsItem {
    pub fn metadata_item(&self) -> MetadataItemId {
        match self {
            Self::Track(uri) => MetadataItemId::Track(uri.clone()),
            Self::Album(uri) => MetadataItemId::Album(uri.clone()),
            Self::Artist { media_uri, .. } => MetadataItemId::Artist(media_uri.clone()),
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MediaDetailField {
    pub label: String,
    pub value: String,
    pub copy: bool,
    pub musicbrainz_url: Option<String>,
    pub links: Vec<MediaDetailText>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MediaDetailText {
    pub text: String,
    pub target: Option<MediaDetailsItem>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MediaDetailSection {
    pub title: String,
    pub fields: Vec<MediaDetailField>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MediaDetails {
    pub heading: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub can_edit: bool,
    pub audio_summary: Vec<MediaDetailField>,
    pub sections: Vec<MediaDetailSection>,
}

pub async fn media_details(
    source: SourceHandle,
    playback: Arc<PlaybackOwner>,
    item: MediaDetailsItem,
) -> LibraryResult<MediaDetails> {
    let runtime = source.shared.runtime.clone();
    super::on_runtime(runtime, async {
        let db = &source.shared.database;
        let cancel = ReadCancellation::new();
        let unavailable = || LibraryError::InvalidRequest(tr(msgid("Item is no longer available")));
        let mut origin = section(msgid("Source"));
        let mut ids = section(msgid("MusicBrainz"));
        let (mut details, source_id, object_id, state) = match &item {
            MediaDetailsItem::Track(uri) => {
                let track = db.track(uri, &cancel).await?.ok_or_else(unavailable)?;
                let mut details = MediaDetails {
                    heading: tr(msgid("Track details")),
                    title: track.metadata.title.clone(),
                    subtitle: [
                        track.metadata.artist.as_str(),
                        track.metadata.album.as_str(),
                    ]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
                    artwork_binding: track.artwork_binding.clone(),
                    can_edit: track.access.cue_path.is_none()
                        && (track.source_key.is_some() || uri.starts_with("file:")),
                    audio_summary: Vec::new(),
                    sections: Vec::new(),
                };
                let mut general = section(msgid("General"));
                for field in [
                    LibraryField::Title,
                    LibraryField::Artist,
                    LibraryField::Album,
                    LibraryField::AlbumArtist,
                    LibraryField::Genre,
                    LibraryField::ReleaseDate,
                    LibraryField::Year,
                    LibraryField::TrackNumber,
                    LibraryField::DiscNumber,
                    LibraryField::Bpm,
                    LibraryField::Duration,
                ] {
                    let mut value = detail_track_field(&track, field);
                    if field == LibraryField::TrackNumber {
                        value = (track.metadata.track_number > 0)
                            .then(|| track.metadata.track_number.to_string())
                            .unwrap_or_default();
                    }
                    if field == LibraryField::DiscNumber && track.metadata.disc_number <= 0 {
                        value.clear();
                    }
                    let mut fact = field_value(field, value.clone());
                    set_links(
                        &mut fact,
                        metadata_links(
                            field,
                            &value,
                            track.links.album_media_uri.as_deref(),
                            &track.links.artists,
                            &track.links.album_artists,
                        ),
                    );
                    push_field(&mut general, fact);
                }
                details.sections.push(general);
                let mut audio = section(msgid("Original audio"));
                for field in [
                    LibraryField::Format,
                    LibraryField::Bitrate,
                    LibraryField::SampleRate,
                    LibraryField::BitDepth,
                    LibraryField::Channels,
                    LibraryField::Duration,
                ] {
                    let value = if field == LibraryField::Format {
                        presentation::audio_source_label(
                            track.metadata.source_format.as_deref(),
                            track.access.source_path.as_deref(),
                        )
                        .unwrap_or_default()
                    } else {
                        detail_track_field(&track, field)
                    };
                    let fact = field_value(field, value);
                    if !fact.value.is_empty()
                        && matches!(
                            field,
                            LibraryField::Format
                                | LibraryField::Bitrate
                                | LibraryField::SampleRate
                                | LibraryField::BitDepth
                        )
                    {
                        details.audio_summary.push(fact.clone());
                    }
                    push_field(&mut audio, fact);
                }
                details.sections.push(audio);
                if let Some(current) = playback
                    .current_media()
                    .filter(|current| current.id.media_uri == *uri)
                    && let Some(stream) = &current.stream
                {
                    let mut playing = section(msgid("Playing stream"));
                    add(
                        &mut playing,
                        msgid("Format"),
                        presentation::audio_source_label(stream.format.as_deref(), None),
                    );
                    add(
                        &mut playing,
                        msgid("Delivery"),
                        Some(tr(if stream.local {
                            msgid("Local")
                        } else {
                            msgid("Streaming")
                        })),
                    );
                    add(
                        &mut playing,
                        msgid("Transcoded"),
                        Some(tr(if stream.transcoded {
                            msgid("Yes")
                        } else {
                            msgid("No")
                        })),
                    );
                    details.sections.push(playing);
                }
                let mut loudness = section(msgid("ReplayGain tags"));
                for (gain, gain_title, peak_title) in [
                    (
                        track.metadata.source_loudness,
                        msgid("Track gain"),
                        msgid("Track peak"),
                    ),
                    (
                        track.metadata.album_loudness,
                        msgid("Album gain"),
                        msgid("Album peak"),
                    ),
                ] {
                    if let Some(gain) = gain {
                        add(
                            &mut loudness,
                            gain_title,
                            gain.replay_gain_db.map(|v| format!("{v:.2} dB")),
                        );
                        add(
                            &mut loudness,
                            peak_title,
                            gain.replay_gain_peak.map(|v| format!("{v:.4}")),
                        );
                    }
                }
                details.sections.push(loudness);
                add(
                    &mut origin,
                    msgid("Source"),
                    Some(track.access.source_name.clone()),
                );
                copy(
                    &mut origin,
                    msgid("File path"),
                    track.access.source_path.clone(),
                );
                if let Some(size) = db.media_file_size(uri).await? {
                    add(
                        &mut origin,
                        if track.access.cue_path.is_some() {
                            msgid("Shared file size")
                        } else {
                            msgid("File size")
                        },
                        Some(glib::format_size(size.max(0) as u64).to_string()),
                    );
                }
                copy(
                    &mut origin,
                    msgid("CUE file"),
                    track.access.cue_path.clone(),
                );
                if let Some(start) = track.access.cue_start_millis {
                    add(
                        &mut origin,
                        msgid("Segment start"),
                        Some(format_duration_units((start.max(0) / 1000) as u32)),
                    );
                }
                if let Some(end) = track.access.cue_end_millis {
                    add(
                        &mut origin,
                        msgid("Segment end"),
                        Some(format_duration_units((end.max(0) / 1000) as u32)),
                    );
                }
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz recording ID"),
                    "recording",
                    track.metadata.musicbrainz_recording_id.clone(),
                );
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz release track ID"),
                    "track",
                    track.metadata.musicbrainz_release_track_id.clone(),
                );
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz release ID"),
                    "release",
                    track.metadata.musicbrainz_album_id.clone(),
                );
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz release group ID"),
                    "release-group",
                    track.metadata.musicbrainz_release_group_id.clone(),
                );
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz artist ID"),
                    "artist",
                    track.metadata.primary_artist_musicbrainz_id.clone(),
                );
                (details, track.source_id, track.object_id, track.state)
            }
            MediaDetailsItem::Album(uri) => {
                let album = db
                    .album(uri, true, &cancel)
                    .await?
                    .ok_or_else(unavailable)?;
                let mut general = section(msgid("General"));
                add(
                    &mut general,
                    msgid("Title"),
                    Some(album.metadata.title.clone()),
                );
                let mut artists = field_value(LibraryField::AlbumArtist, String::new());
                artists.label = tr(msgid("Album artists"));
                set_links(&mut artists, album_artist_links(&album));
                push_field(&mut general, artists);
                add(
                    &mut general,
                    msgid("Release date"),
                    album.metadata.release_date.clone(),
                );
                add(
                    &mut general,
                    msgid("Year"),
                    album.metadata.year.map(|v| v.to_string()),
                );
                add(
                    &mut general,
                    msgid("Release type"),
                    Some(album.metadata.release_types.join(", ")),
                );
                add(
                    &mut general,
                    msgid("Genre"),
                    Some(
                        album
                            .links
                            .genres
                            .iter()
                            .map(|genre| genre.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                );
                add_summary(&mut general, &album.summary);
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz release ID"),
                    "release",
                    album.metadata.musicbrainz_release_id.clone(),
                );
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz release group ID"),
                    "release-group",
                    album.metadata.musicbrainz_release_group_id.clone(),
                );
                let details = MediaDetails {
                    heading: tr(msgid("Album details")),
                    title: album.metadata.title.clone(),
                    subtitle: album.metadata.artist.clone(),
                    artwork_binding: album.artwork_binding,
                    can_edit: true,
                    audio_summary: Vec::new(),
                    sections: vec![general],
                };
                (
                    details,
                    library::source_entity_parts(uri).map(|(id, _, _)| id.to_string()),
                    Some(album.object_id),
                    album.state,
                )
            }
            MediaDetailsItem::Artist {
                media_uri: uri,
                album_artist,
            } => {
                let artist = db
                    .artist(uri, *album_artist, true, &cancel)
                    .await?
                    .ok_or_else(unavailable)?;
                let mut general = section(msgid("General"));
                add(
                    &mut general,
                    msgid("Name"),
                    Some(artist.metadata.name.clone()),
                );
                add(
                    &mut general,
                    msgid("Sort name"),
                    artist.metadata.sort_text.clone(),
                );
                add_summary(&mut general, &artist.summary);
                musicbrainz(
                    &mut ids,
                    msgid("MusicBrainz artist ID"),
                    "artist",
                    artist.metadata.musicbrainz_artist_id.clone(),
                );
                let details = MediaDetails {
                    heading: tr(msgid("Artist details")),
                    title: artist.metadata.name.clone(),
                    subtitle: String::new(),
                    artwork_binding: artist.artwork_binding,
                    can_edit: true,
                    audio_summary: Vec::new(),
                    sections: vec![general],
                };
                (
                    details,
                    library::source_entity_parts(uri).map(|(id, _, _)| id.to_string()),
                    Some(artist.object_id),
                    artist.state,
                )
            }
        };
        let settings = source.shared.settings.load();
        if let Some(configured) = settings.sources.connections().find(|configured| {
            Some(configured.configuration.source_id.as_str()) == source_id.as_deref()
        }) {
            if origin.fields.is_empty() {
                add(
                    &mut origin,
                    msgid("Source"),
                    Some(configured.configuration.name.clone()),
                );
            }
            copy(
                &mut origin,
                msgid("Server"),
                configured.configuration.server_address(),
            );
        }
        copy(&mut origin, msgid("Item ID"), object_id);
        let mut listening = section(msgid("Listening"));
        add_state(&mut listening, &state);
        details.sections.insert(1, listening);
        details.sections.push(origin);
        details.sections.push(ids);
        details
            .sections
            .retain(|section| !section.fields.is_empty());
        Ok(details)
    })
    .await
}

fn set_links(field: &mut MediaDetailField, links: DetailLinks) {
    field.value = links.display_text().to_owned();
    let mut cursor = 0;
    for link in links.spans() {
        if cursor < link.range.start {
            field.links.push(MediaDetailText {
                text: field
                    .value
                    .get(cursor..link.range.start)
                    .expect("detail link ranges stay on text boundaries")
                    .to_owned(),
                target: None,
            });
        }
        let target = match &link.route {
            Route::AlbumDetail(uri) => MediaDetailsItem::Album(uri.clone()),
            Route::ArtistDetail(uri) => MediaDetailsItem::Artist {
                media_uri: uri.clone(),
                album_artist: false,
            },
            Route::AlbumArtistDetail(uri) => MediaDetailsItem::Artist {
                media_uri: uri.clone(),
                album_artist: true,
            },
            _ => unreachable!("metadata links only target albums and artists"),
        };
        field.links.push(MediaDetailText {
            text: field
                .value
                .get(link.range.clone())
                .expect("detail link ranges stay on text boundaries")
                .to_owned(),
            target: Some(target),
        });
        cursor = link.range.end;
    }
    if !field.links.is_empty() && cursor < field.value.len() {
        field.links.push(MediaDetailText {
            text: field
                .value
                .get(cursor..)
                .expect("detail link ranges stay on text boundaries")
                .to_owned(),
            target: None,
        });
    }
}

fn detail_track_field(track: &library::TrackView, field: LibraryField) -> String {
    if field == LibraryField::Duration {
        return (track.metadata.duration_millis > 0)
            .then(|| format_duration_units((track.metadata.duration_millis / 1000) as u32))
            .unwrap_or_default();
    }
    presentation::track_field(track, field)
}

fn musicbrainz(section: &mut MediaDetailSection, label: &str, entity: &str, value: Option<String>) {
    if let Some(value) = value {
        let url = format!("https://musicbrainz.org/{entity}/{}", value.trim());
        push_field(
            section,
            MediaDetailField {
                label: tr(label),
                value,
                copy: true,
                musicbrainz_url: Some(url),
                links: Vec::new(),
            },
        );
    }
}

fn section(title: &str) -> MediaDetailSection {
    MediaDetailSection {
        title: tr(title),
        fields: Vec::new(),
    }
}
fn field_value(field: LibraryField, value: String) -> MediaDetailField {
    MediaDetailField {
        label: tr(presentation::library_field_title(field)),
        value,
        copy: false,
        musicbrainz_url: None,
        links: Vec::new(),
    }
}
fn push_field(section: &mut MediaDetailSection, field: MediaDetailField) {
    if !field.value.trim().is_empty() {
        section.fields.push(field);
    }
}
fn add(section: &mut MediaDetailSection, label: &str, value: Option<String>) {
    if let Some(value) = value {
        push_field(
            section,
            MediaDetailField {
                label: tr(label),
                value,
                copy: false,
                musicbrainz_url: None,
                links: Vec::new(),
            },
        );
    }
}
fn copy(section: &mut MediaDetailSection, label: &str, value: Option<String>) {
    if let Some(value) = value {
        push_field(
            section,
            MediaDetailField {
                label: tr(label),
                value,
                copy: true,
                musicbrainz_url: None,
                links: Vec::new(),
            },
        );
    }
}
fn add_state(section: &mut MediaDetailSection, state: &TrackState) {
    add(
        section,
        presentation::library_field_title(LibraryField::UserRating),
        state.rating.map(|_| stored_rating(state.rating)),
    );
    add(
        section,
        msgid("Play count"),
        Some(state.play_count.to_string()),
    );
    add(
        section,
        msgid("Last played"),
        state
            .last_played
            .map(|_| display_unix_date(state.last_played)),
    );
}
fn add_summary(section: &mut MediaDetailSection, summary: &library::CollectionSummary) {
    add(
        section,
        msgid("Tracks"),
        Some(summary.track_count.to_string()),
    );
    if summary.album_count > 0 {
        add(
            section,
            msgid("Albums"),
            Some(summary.album_count.to_string()),
        );
    }
    add(
        section,
        msgid("Duration"),
        Some(format_duration_units(
            (summary.duration_millis.max(0) / 1000) as u32,
        )),
    );
}
