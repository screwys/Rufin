use localization::msgid;
use rufin_core::settings::{HomeBlockKind, LibraryField, LibraryListKey};

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
        LibraryListKey::Albums => msgid("Albums"),
        LibraryListKey::Artists => msgid("Artists"),
        LibraryListKey::AlbumArtists => msgid("Album artists"),
        LibraryListKey::Tracks => msgid("Tracks"),
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
        LibraryField::TrackNumber => msgid("Track"),
        LibraryField::DiscNumber => msgid("Disc"),
        LibraryField::SongCount => msgid("Number of songs"),
        LibraryField::AlbumCount => msgid("Albums"),
        LibraryField::Duration => msgid("Duration"),
        LibraryField::Favorite => msgid("Favorite"),
    }
}
