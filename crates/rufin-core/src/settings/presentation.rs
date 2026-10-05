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

use library::{TrackArtistLink, TrackRow};

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

pub fn track_field(track: &TrackRow, field: LibraryField) -> String {
    match field {
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&track.audio_properties, field),
        LibraryField::Format => track.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => track.source_path.clone().unwrap_or_default(),
        LibraryField::Source => track.source_name.clone(),
        LibraryField::Title | LibraryField::TitleMerged => track.title.clone(),
        LibraryField::Artist => track.artist.clone(),
        LibraryField::AlbumArtist => joined_credits(&track.album_artists),
        LibraryField::Album => track.album.clone(),
        LibraryField::Year => optional_year(track.year),
        LibraryField::ReleaseDate => track.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => track.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(track.last_played),
        LibraryField::PlayCount => count(track.play_count),
        LibraryField::UserRating => stored_rating(track.rating),
        LibraryField::Genre => track
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Bpm => track.bpm.map(|bpm| bpm.to_string()).unwrap_or_default(),
        LibraryField::DiscNumber => track.disc_number.to_string(),
        LibraryField::TrackNumber => {
            optional_track_number(Some(track.disc_number), Some(track.track_number))
        }
        LibraryField::Duration => format_duration((track.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(track.favorite),
        _ => String::new(),
    }
}

pub fn playlist_entry_field(entry: &library::PlaylistEntryRow, field: LibraryField) -> String {
    match field {
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&entry.audio_properties, field),
        LibraryField::Format => entry.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => entry.source_path.clone().unwrap_or_default(),
        LibraryField::Source => entry.source_name.clone().unwrap_or_default(),
        LibraryField::Title | LibraryField::TitleMerged => entry.title.clone(),
        LibraryField::Artist => entry.artist.clone(),
        LibraryField::AlbumArtist => entry
            .album_display_artist
            .clone()
            .unwrap_or_else(|| entry.artist.clone()),
        LibraryField::Album => entry.album.clone(),
        LibraryField::Year => optional_year(entry.year),
        LibraryField::ReleaseDate => entry.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => entry.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(entry.last_played),
        LibraryField::PlayCount => count(entry.play_count),
        LibraryField::Genre => entry.genre.clone(),
        LibraryField::Bpm => entry.bpm.map(|value| value.to_string()).unwrap_or_default(),
        LibraryField::UserRating => stored_rating(entry.rating),
        LibraryField::DiscNumber => entry
            .disc_number
            .map(|number| number.to_string())
            .unwrap_or_default(),
        LibraryField::TrackNumber => optional_track_number(entry.disc_number, entry.track_number),
        LibraryField::Duration => format_duration((entry.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(entry.favorite),
        _ => String::new(),
    }
}

pub fn smart_track_field(row: &library::SmartPlaylistTrackRow, field: LibraryField) -> String {
    match field {
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&row.audio_properties, field),
        LibraryField::Format => row.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => row.source_path.clone().unwrap_or_default(),
        LibraryField::Source => row.source_name.clone().unwrap_or_default(),
        LibraryField::Title | LibraryField::TitleMerged => row.title.clone(),
        LibraryField::Artist => row.artist.clone(),
        LibraryField::Album => row.album.clone(),
        LibraryField::AlbumArtist => row.album_display_artist.clone().unwrap_or_default(),
        LibraryField::Year => optional_year(row.year),
        LibraryField::ReleaseDate => row.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => row.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(row.last_played),
        LibraryField::PlayCount => count(row.play_count),
        LibraryField::UserRating => stored_rating(row.rating),
        LibraryField::Genre => row.genre.clone(),
        LibraryField::Bpm => row.bpm.map(|value| value.to_string()).unwrap_or_default(),
        LibraryField::DiscNumber => row
            .disc_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        LibraryField::TrackNumber => optional_track_number(row.disc_number, row.track_number),
        LibraryField::Duration => format_duration((row.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(row.favorite),
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

pub fn history_field(row: &library::HistoryRow, field: LibraryField) -> String {
    match field {
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&row.audio_properties, field),
        LibraryField::Format => row.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => row.source_path.clone().unwrap_or_default(),
        LibraryField::Source => row.source_name.clone().unwrap_or_default(),
        LibraryField::Title | LibraryField::TitleMerged => row.title.clone(),
        LibraryField::Artist => row.artist.clone(),
        LibraryField::Album => row.album.clone(),
        LibraryField::AlbumArtist => row.album_display_artist.clone().unwrap_or_default(),
        LibraryField::Year => optional_year(row.year),
        LibraryField::ReleaseDate => row.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => row.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(row.last_played),
        LibraryField::PlayCount => count(row.play_count),
        LibraryField::Genre => row.genre.clone(),
        LibraryField::Bpm => row.bpm.map(|value| value.to_string()).unwrap_or_default(),
        LibraryField::UserRating => stored_rating(row.rating),
        LibraryField::DiscNumber => row
            .disc_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        LibraryField::TrackNumber => optional_track_number(row.disc_number, row.track_number),
        LibraryField::Duration => format_duration((row.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(row.favorite),
        _ => String::new(),
    }
}
