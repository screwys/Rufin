use super::{HomeBlockKind, LibraryField, LibraryListKey};

pub fn audio_source_label(format: Option<&str>, path: Option<&str>) -> Option<String> {
    let extension = path
        .map(|path| {
            if path.contains(['?', '#'])
                && url::Url::parse(path)
                    .is_ok_and(|uri| uri.scheme().len() > 1 && !uri.cannot_be_a_base())
            {
                path.split(['?', '#']).next().unwrap_or(path)
            } else {
                path
            }
        })
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .and_then(|path| std::path::Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let value = format
        .map(|format| {
            let mut parts = format.split(';');
            let media_type = parts.next().unwrap_or(format).trim();
            parts
                .find_map(|parameter| {
                    let (key, value) = parameter.trim().split_once('=')?;
                    (key.trim().eq_ignore_ascii_case("codecs"))
                        .then_some(value.trim().trim_matches('"'))
                })
                .unwrap_or(media_type)
        })
        .and_then(|value| value.rsplit('/').next())
        .map(|value| value.trim().trim_start_matches('.'))
        .filter(|value| !value.is_empty())
        .or(extension)?;
    let value = if value.contains(',') {
        extension
            .filter(|extension| {
                value
                    .split(',')
                    .any(|alias| alias.trim().eq_ignore_ascii_case(extension))
            })
            .unwrap_or_else(|| value.split(',').next().unwrap_or(value).trim())
    } else {
        value
    };
    if value.is_empty() {
        return None;
    }
    Some(match value.to_ascii_lowercase().as_str() {
        "mpeg" | "mpga" => "MP3".to_string(),
        other => other.to_ascii_uppercase(),
    })
}

pub fn now_playing_labels(
    media: Option<&playback::CurrentMedia>,
    output_name: Option<&str>,
) -> Vec<String> {
    let Some(media) = media else {
        return Vec::new();
    };
    let mut labels = Vec::new();
    let original = audio_source_label(
        media.row.track.metadata.source_format.as_deref(),
        Some(&media.row.track.media_uri),
    );
    let same_format = media.stream.as_ref().is_none_or(|stream| {
        !stream.transcoded
            && stream.format.as_ref().is_none_or(|format| {
                original
                    .as_deref()
                    .and_then(crate::playback::audio_mime)
                    .is_some_and(|original| {
                        original == *format || original.split(';').next() == Some(format.as_str())
                    })
            })
    });
    let format = if same_format {
        original
    } else {
        media
            .stream
            .as_ref()
            .and_then(|stream| audio_source_label(stream.format.as_deref(), None))
    };
    labels.extend(
        media
            .row
            .track
            .metadata
            .year
            .filter(|year| *year > 0)
            .map(|year| year.to_string()),
    );
    // One label for the audio format and one for where it plays.
    let mut audio = Vec::from_iter(format);
    if same_format {
        let properties = &media.row.track.metadata.audio_properties;
        audio.extend(
            properties
                .bit_depth
                .filter(|bits| *bits > 0)
                .map(|bits| format!("{bits} bit")),
        );
        audio.extend(
            properties
                .sample_rate_hz
                .filter(|rate| *rate > 0)
                .map(|rate| format!("{} kHz", f64::from(rate) / 1000.0)),
        );
    }
    let mut place = Vec::from_iter(output_name.map(str::to_owned));
    if let Some(stream) = &media.stream {
        place.push(localization::tr(if stream.local {
            msgid("Local")
        } else {
            msgid("Streaming")
        }));
        if stream.bitstream {
            place.push(localization::tr(msgid("Bitstream")));
        }
    }
    labels.extend(
        [audio, place]
            .into_iter()
            .filter(|group| !group.is_empty())
            .map(|group| group.join(" · ")),
    );
    labels
}

pub fn home_provider_title(section_id: &str, title: Option<&str>) -> String {
    title.map(str::to_owned).unwrap_or_else(|| {
        section_id
            .split(['-', '_'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}
use localization::msgid;

pub fn home_block_title(value: HomeBlockKind) -> &'static str {
    match value {
        HomeBlockKind::Showcase => msgid("Showcase"),
        HomeBlockKind::Explore => msgid("Explore"),
        HomeBlockKind::MostPlayed => msgid("Most played"),
        HomeBlockKind::NewlyAdded => msgid("Newly added"),
        HomeBlockKind::RecentlyPlayed => msgid("Recently played"),
        HomeBlockKind::RecentlyReleased => msgid("Recently released"),
        HomeBlockKind::Genres => msgid("Featured genres"),
    }
}

pub fn library_list_title(value: LibraryListKey) -> &'static str {
    match value {
        LibraryListKey::Queue => msgid("Queue"),
        LibraryListKey::FullscreenTracks => msgid("Fullscreen player"),
        LibraryListKey::Albums => msgid("Albums"),
        LibraryListKey::Artists => msgid("Artists"),
        LibraryListKey::AlbumArtists => msgid("Album artists"),
        LibraryListKey::Tracks => msgid("Tracks"),
        LibraryListKey::Folders => msgid("Folders"),
        LibraryListKey::FavoriteTracks => msgid("Favorites"),
        LibraryListKey::History => msgid("History"),
        LibraryListKey::Genres => msgid("Genres"),
        LibraryListKey::Moods => msgid("Moods"),
        LibraryListKey::Playlists => msgid("Playlists"),
        LibraryListKey::SmartPlaylists => msgid("Smart playlists"),
        LibraryListKey::AlbumDetailTracks => msgid("Album tracks"),
        LibraryListKey::ArtistAlbums => msgid("Artist albums"),
        LibraryListKey::ArtistTracks => msgid("Artist tracks"),
        LibraryListKey::GenreTracks => msgid("Genre tracks"),
        LibraryListKey::MoodTracks => msgid("Mood tracks"),
        LibraryListKey::PlaylistTracks => msgid("Playlist tracks"),
        LibraryListKey::SmartPlaylistTracks => msgid("Smart playlist tracks"),
    }
}

pub fn library_field_title(value: LibraryField) -> &'static str {
    match value {
        LibraryField::RowIndex => "#",
        LibraryField::Image => msgid("Image"),
        LibraryField::Title => msgid("Title"),
        LibraryField::TitleMerged => msgid("Title (merged)"),
        LibraryField::Artist => msgid("Artist"),
        LibraryField::AlbumArtist => msgid("Album artist"),
        LibraryField::Album => msgid("Album"),
        LibraryField::Year => msgid("Year"),
        LibraryField::ReleaseDate => msgid("Release date"),
        LibraryField::DateAdded => msgid("Date added"),
        LibraryField::LastPlayed => msgid("Last played"),
        LibraryField::PlayCount => msgid("Plays"),
        LibraryField::UserRating => msgid("Rating"),
        LibraryField::Genre => msgid("Genre"),
        LibraryField::Bpm => msgid("BPM"),
        LibraryField::Bitrate => msgid("Bitrate"),
        LibraryField::SampleRate => msgid("Sample rate"),
        LibraryField::BitDepth => msgid("Bit depth"),
        LibraryField::Channels => msgid("Channels"),
        LibraryField::Format => msgid("Format"),
        LibraryField::FilePath => msgid("File path"),
        LibraryField::Source => msgid("Source"),
        LibraryField::TrackNumber => msgid("Track"),
        LibraryField::DiscNumber => msgid("Disc"),
        LibraryField::SongCount => msgid("Number of songs"),
        LibraryField::AlbumCount => msgid("Albums"),
        LibraryField::Duration => msgid("Duration"),
        LibraryField::Favorite => msgid("Favorite"),
        LibraryField::Tools => msgid("Tools"),
    }
}

use library::{TrackArtistLink, TrackView};

pub fn format_duration(seconds: u32) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}:{seconds:02}")
}

pub fn format_duration_units(seconds: u32) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        return format!("{hours}h {minutes}m {seconds}s");
    }
    if minutes > 0 {
        return format!("{minutes}m {seconds}s");
    }
    format!("{seconds}s")
}

pub fn track_field(song: &TrackView, field: LibraryField) -> String {
    let metadata = &song.metadata;
    match field {
        LibraryField::Title | LibraryField::TitleMerged => metadata.title.clone(),
        LibraryField::Artist => metadata.artist.clone(),
        LibraryField::Album => metadata.album.clone(),
        LibraryField::AlbumArtist => joined_credits(&song.links.album_artists),
        LibraryField::Year => optional_year(metadata.year),
        LibraryField::ReleaseDate => metadata.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => metadata.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(song.state.last_played),
        LibraryField::PlayCount => count(song.state.play_count),
        LibraryField::UserRating => stored_rating(song.state.rating),
        LibraryField::Bpm => metadata
            .bpm
            .map(|value| value.to_string())
            .unwrap_or_default(),
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&metadata.audio_properties, field),
        LibraryField::Format => metadata.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => song.access.source_path.clone().unwrap_or_default(),
        LibraryField::Source => song.access.source_name.clone(),
        LibraryField::DiscNumber => metadata.disc_number.to_string(),
        LibraryField::TrackNumber => {
            optional_track_number(Some(metadata.disc_number), Some(metadata.track_number))
        }
        LibraryField::Genre => song
            .links
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Duration => format_duration((metadata.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(song.state.favorite),
        _ => String::new(),
    }
}

pub fn audio_property_field(properties: &library::AudioProperties, field: LibraryField) -> String {
    match field {
        LibraryField::Bitrate => properties
            .bitrate_kbps
            .filter(|value| *value > 0)
            .map(|value| format!("{value} kbps")),
        LibraryField::SampleRate => properties
            .sample_rate_hz
            .filter(|value| *value > 0)
            .map(|value| format!("{} kHz", f64::from(value) / 1000.0)),
        LibraryField::BitDepth => properties
            .bit_depth
            .filter(|value| *value > 0)
            .map(|value| value.to_string()),
        LibraryField::Channels => properties
            .channels
            .filter(|value| *value > 0)
            .map(|value| value.to_string()),
        _ => None,
    }
    .unwrap_or_default()
}

pub fn optional_track_number(disc: Option<i64>, track: Option<i64>) -> String {
    match (disc, track) {
        (Some(disc), Some(track)) => format!("{disc}-{track:02}"),
        (_, Some(track)) => track.to_string(),
        _ => String::new(),
    }
}

pub fn count(value: i64) -> String {
    value.max(0).to_string()
}

pub fn stored_rating(value: Option<i64>) -> String {
    value
        .map(|value| format!("{:.1}", value as f64 / 2.0))
        .unwrap_or_default()
}

pub fn optional_year(year: Option<i64>) -> String {
    year.filter(|year| *year != 0)
        .map(|year| year.to_string())
        .unwrap_or_default()
}

pub fn display_unix_date(value: Option<i64>) -> String {
    value
        .and_then(|value| glib::DateTime::from_unix_local(value).ok())
        .and_then(|value| value.format("%Y-%m-%d %H:%M").ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}
pub fn favorite_text(favorite: bool) -> String {
    if favorite { "♥" } else { "" }.to_string()
}
pub fn joined_credits(credits: &[TrackArtistLink]) -> String {
    credits
        .iter()
        .map(|credit| credit.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
