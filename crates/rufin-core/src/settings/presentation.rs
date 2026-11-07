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

/// An album page's name for its release kind.
pub fn album_release_title(value: library::AlbumReleaseClass) -> &'static str {
    match value {
        library::AlbumReleaseClass::Album => msgid("Album"),
        library::AlbumReleaseClass::Ep => msgid("EP"),
        library::AlbumReleaseClass::Single => msgid("Single"),
        library::AlbumReleaseClass::Collection => msgid("Collection"),
        library::AlbumReleaseClass::Other => msgid("Release"),
    }
}

/// An artist page's release groups in their order: each release class, then Appears On.
pub const ARTIST_RELEASE_TITLES: [&str; 6] = [
    msgid("Albums"),
    msgid("EPs"),
    msgid("Singles"),
    msgid("Collections"),
    msgid("Other releases"),
    msgid("Appears On"),
];

/// The title a list's sort choice shows for `field`.
pub fn library_sort_title(key: LibraryListKey, field: LibraryField) -> &'static str {
    if key == LibraryListKey::PlaylistTracks && field == LibraryField::RowIndex {
        msgid("Playlist order")
    } else {
        library_field_title(field)
    }
}

/// The title of the fields a layout shows.
pub fn library_fields_title(layout: super::LibraryLayout) -> &'static str {
    match layout {
        super::LibraryLayout::Row => msgid("Columns"),
        super::LibraryLayout::Grid => msgid("Grid labels"),
        super::LibraryLayout::Detail => msgid("Detail track columns"),
    }
}

/// A list column's header: the field's title, "Title" for the merged title, and none for
/// the tools column.
pub fn library_column_title(value: LibraryField) -> Option<&'static str> {
    match value {
        LibraryField::Tools => None,
        LibraryField::TitleMerged => Some(msgid("Title")),
        field => Some(library_field_title(field)),
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

use library::{
    SmartPlaylistActivityPeriod, SmartPlaylistRuleField, SmartPlaylistRuleOperator,
    SmartPlaylistSort,
};

pub fn smart_rule_field_title(field: SmartPlaylistRuleField) -> &'static str {
    match field {
        SmartPlaylistRuleField::Title => msgid("Title"),
        SmartPlaylistRuleField::Artist => msgid("Artist"),
        SmartPlaylistRuleField::Album => msgid("Album"),
        SmartPlaylistRuleField::Comment => msgid("Comment"),
        SmartPlaylistRuleField::Genre => msgid("Genre"),
        SmartPlaylistRuleField::Mood => msgid("Mood"),
        SmartPlaylistRuleField::Bpm => msgid("BPM"),
        SmartPlaylistRuleField::Rating => msgid("Rating"),
        SmartPlaylistRuleField::Year => msgid("Year"),
        SmartPlaylistRuleField::Favorite => msgid("Favorite"),
        SmartPlaylistRuleField::Played => msgid("Ever played"),
        SmartPlaylistRuleField::PlayCount => msgid("Play count"),
        SmartPlaylistRuleField::SkipCount => msgid("Skip count"),
        SmartPlaylistRuleField::LastPlayed => msgid("Last played"),
        SmartPlaylistRuleField::DateAdded => msgid("Date added"),
    }
}

pub fn smart_sort_title(field: SmartPlaylistSort) -> &'static str {
    match field {
        SmartPlaylistSort::Title => msgid("Title"),
        SmartPlaylistSort::Artist => msgid("Artist"),
        SmartPlaylistSort::Album => msgid("Album"),
        SmartPlaylistSort::Year => msgid("Year"),
        SmartPlaylistSort::DateAdded => msgid("Date added"),
        SmartPlaylistSort::LastPlayed => msgid("Last played"),
        SmartPlaylistSort::PlayCount => msgid("Play count"),
        SmartPlaylistSort::SkipCount => msgid("Skip count"),
        SmartPlaylistSort::Bpm => msgid("BPM"),
        SmartPlaylistSort::Rating => msgid("Rating"),
        SmartPlaylistSort::Duration => msgid("Duration"),
    }
}

pub fn smart_operator_title(
    field: SmartPlaylistRuleField,
    operator: SmartPlaylistRuleOperator,
) -> &'static str {
    match (field, operator) {
        (
            SmartPlaylistRuleField::Genre | SmartPlaylistRuleField::Mood,
            SmartPlaylistRuleOperator::NotContains,
        ) => msgid("excludes"),
        (
            SmartPlaylistRuleField::Genre | SmartPlaylistRuleField::Mood,
            SmartPlaylistRuleOperator::NotEquals,
        ) => msgid("is not"),
        (_, SmartPlaylistRuleOperator::Contains) => msgid("contains"),
        (_, SmartPlaylistRuleOperator::NotContains) => msgid("does not contain"),
        (_, SmartPlaylistRuleOperator::Equals) => msgid("equals"),
        (_, SmartPlaylistRuleOperator::NotEquals) => msgid("does not equal"),
        (_, SmartPlaylistRuleOperator::Above) => msgid("above"),
        (_, SmartPlaylistRuleOperator::Below) => msgid("below"),
        (_, SmartPlaylistRuleOperator::Between) => msgid("range"),
        (_, SmartPlaylistRuleOperator::Is) => msgid("is"),
        (_, SmartPlaylistRuleOperator::IsNot) => msgid("is not"),
        (_, SmartPlaylistRuleOperator::Before) => msgid("before"),
        (_, SmartPlaylistRuleOperator::After) => msgid("after"),
        (_, SmartPlaylistRuleOperator::IsEmpty) => msgid("is empty"),
        (_, SmartPlaylistRuleOperator::IsNotEmpty) => msgid("is not empty"),
    }
}

pub fn smart_period_title(period: SmartPlaylistActivityPeriod) -> &'static str {
    match period {
        SmartPlaylistActivityPeriod::Weekly => msgid("Last 7 days"),
        SmartPlaylistActivityPeriod::Monthly => msgid("Last 30 days"),
        SmartPlaylistActivityPeriod::Yearly => msgid("Last 365 days"),
        SmartPlaylistActivityPeriod::Lifetime => msgid("All time"),
    }
}

use library::{TrackArtistLink, TrackRow, TrackView};
use localization::{album_count_text, track_count_text};

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

/// A track's text for one field, as its list row shows it.
pub fn track_field(song: &TrackView, field: LibraryField) -> String {
    track_row_field(&library::TrackRow::from(song.clone()), field)
}

/// A list row's text for one field.
pub fn track_row_field(row: &TrackRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => row.title.clone(),
        LibraryField::Artist => row.artist.clone(),
        LibraryField::Album => row.album.clone(),
        LibraryField::AlbumArtist => joined_credits(&row.links.album_artists),
        LibraryField::Year => optional_year(row.year),
        LibraryField::ReleaseDate => row.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => row
            .date_added
            .and_then(|seconds| glib::DateTime::from_unix_utc(seconds).ok())
            .and_then(|date| date.format("%Y-%m-%d").ok())
            .map(|date| date.to_string())
            .unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(row.state.last_played),
        LibraryField::PlayCount => count(row.state.play_count),
        LibraryField::UserRating => stored_rating(row.state.rating),
        LibraryField::Bpm => row.bpm.map(|value| value.to_string()).unwrap_or_default(),
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels => audio_property_field(&row.audio, field),
        LibraryField::Format => row.source_format.clone().unwrap_or_default(),
        LibraryField::FilePath => row.source_path.clone().unwrap_or_default(),
        LibraryField::Source => row.source_name.clone(),
        LibraryField::DiscNumber => row.disc_number.to_string(),
        LibraryField::TrackNumber => {
            optional_track_number(Some(row.disc_number), Some(row.track_number))
        }
        LibraryField::Genre => row
            .links
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Duration => format_duration((row.duration_millis.max(0) / 1_000) as u32),
        LibraryField::Favorite => favorite_text(row.state.favorite),
        _ => String::new(),
    }
}

/// A list row's text for one field: what its card label or column shows.
pub fn row_field(row: &library::Row, field: LibraryField) -> String {
    use library::{PlaylistListRow, Row};
    match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => track_row_field(track, field),
        Row::Album(album) | Row::AlbumHeading(album) => album_field(album, field),
        Row::Artist(artist) => artist_field(artist, field),
        Row::Tag(tag) => tag_field(tag, field),
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => playlist_field(playlist, field),
        Row::Playlist(PlaylistListRow::SmartPlaylist(playlist)) => {
            smart_playlist_field(playlist, field)
        }
        Row::Playlist(PlaylistListRow::Folder(folder)) => playlist_folder_field(folder, field),
        Row::Folder(folder) => match field {
            LibraryField::Title | LibraryField::TitleMerged => folder.name.clone(),
            _ => String::new(),
        },
    }
}

pub fn album_field(album: &library::AlbumRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => album.title.clone(),
        LibraryField::AlbumArtist | LibraryField::Artist => album.artist.clone(),
        LibraryField::Year => optional_year(album.year),
        LibraryField::ReleaseDate => album.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => album.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(album.state.last_played),
        LibraryField::PlayCount => count(album.state.play_count),
        LibraryField::UserRating => stored_rating(album.state.rating),
        LibraryField::Genre => album
            .links
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Favorite => favorite_text(album.state.favorite),
        LibraryField::SongCount => track_count_text(album.summary.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration((album.summary.duration_millis.max(0) / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub fn artist_field(artist: &library::ArtistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => artist.name.clone(),
        LibraryField::LastPlayed => display_unix_date(artist.state.last_played),
        LibraryField::PlayCount => count(artist.state.play_count),
        LibraryField::UserRating => stored_rating(artist.state.rating),
        LibraryField::Favorite => favorite_text(artist.state.favorite),
        LibraryField::AlbumCount => album_count_text(artist.summary.album_count.max(0) as u64),
        LibraryField::SongCount => track_count_text(artist.summary.track_count.max(0) as u64),
        _ => String::new(),
    }
}

pub fn tag_field(tag: &library::TagRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => tag.name.clone(),
        LibraryField::AlbumCount if tag.kind == library::TagKind::Genre => {
            album_count_text(tag.summary.album_count.max(0) as u64)
        }
        LibraryField::SongCount => track_count_text(tag.summary.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration_units((tag.summary.duration_millis.max(0) / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub fn playlist_field(playlist: &library::PlaylistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => playlist.name.clone(),
        LibraryField::SongCount => track_count_text(playlist.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub fn smart_playlist_field(playlist: &library::SmartPlaylistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => {
            crate::playlists::smart_playlist_display_name(&playlist.object_id, &playlist.name)
        }
        LibraryField::SongCount if playlist.track_count > 0 => {
            track_count_text(playlist.track_count as u64)
        }
        LibraryField::Duration if playlist.duration_millis > 0 => {
            format_duration_units((playlist.duration_millis / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub fn playlist_folder_field(folder: &library::PlaylistFolderRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => folder.name.clone(),
        LibraryField::SongCount => track_count_text(folder.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration_units((folder.duration_millis.max(0) / 1_000) as u32)
        }
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

#[cfg(test)]
mod tests {
    #[test]
    fn display_unix_date_shows_date_and_time() {
        let dt = glib::DateTime::from_local(2026, 9, 22, 14, 30, 0.0).unwrap();
        assert_eq!(
            super::display_unix_date(Some(dt.to_unix())),
            "2026-09-22 14:30"
        );
        assert_eq!(super::display_unix_date(None), "");
    }

    #[test]
    fn formats_track_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(185), "3:05");
        assert_eq!(super::format_duration(3_661), "61:01");
    }

    #[test]
    fn formats_duration_units() {
        assert_eq!(super::format_duration_units(41), "41s");
        assert_eq!(super::format_duration_units(743), "12m 23s");
        assert_eq!(super::format_duration_units(4_421), "1h 13m 41s");
    }
}
