package io.github.screwys.rufin.browse

import io.github.screwys.rufin.core.AndroidBrowseItem
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.shared.library.sourceIdForUri
import io.github.screwys.rufin.shared.rufin_core.RecentSearchKind
import io.github.screwys.rufin.shared.rufin_core.Route

internal val AndroidBrowseRow.kind: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> "track"
        is AndroidBrowseItem.History -> "track"
        is AndroidBrowseItem.PlaylistEntry -> "track"
        is AndroidBrowseItem.Queued -> "track"
        is AndroidBrowseItem.Album -> if (albumHeader) "album_header" else "album"
        is AndroidBrowseItem.Artist -> "artist"
        is AndroidBrowseItem.Genre -> "genre"
        is AndroidBrowseItem.HomeGenre -> "genre"
        is AndroidBrowseItem.Mood -> "mood"
        is AndroidBrowseItem.Playlist -> "playlist"
        is AndroidBrowseItem.SmartPlaylist -> "smart_playlist"
        is AndroidBrowseItem.Folder -> "folder"
        is AndroidBrowseItem.Recent -> item.row.kind.name.lowercase()
    }

internal val AndroidBrowseRow.key: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.mediaUri
        is AndroidBrowseItem.History -> item.row.mediaUri
        is AndroidBrowseItem.PlaylistEntry -> item.row.playlistEntryKey.toString()
        is AndroidBrowseItem.Queued -> item.row.mediaUri
        is AndroidBrowseItem.Album -> item.row.objectId
        is AndroidBrowseItem.Artist -> item.row.objectId
        is AndroidBrowseItem.Genre -> item.row.objectId
        is AndroidBrowseItem.HomeGenre -> item.row.objectId
        is AndroidBrowseItem.Mood -> item.row.moodKey.toString()
        is AndroidBrowseItem.Playlist -> item.row.objectId
        is AndroidBrowseItem.SmartPlaylist -> item.row.objectId
        is AndroidBrowseItem.Recent -> item.row.mediaUri
        is AndroidBrowseItem.Folder -> item.key
    }

internal val AndroidBrowseRow.mediaUri: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.mediaUri
        is AndroidBrowseItem.History -> item.row.mediaUri
        is AndroidBrowseItem.PlaylistEntry -> item.row.mediaUri
        is AndroidBrowseItem.Queued -> item.row.mediaUri
        is AndroidBrowseItem.Album -> item.row.mediaUri
        is AndroidBrowseItem.Artist -> item.row.mediaUri
        is AndroidBrowseItem.Recent -> item.row.mediaUri
        else -> ""
    }

internal val AndroidBrowseRow.title: String
    get() = displayTitle ?: when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.title
        is AndroidBrowseItem.History -> item.row.title
        is AndroidBrowseItem.PlaylistEntry -> item.row.title
        is AndroidBrowseItem.Queued -> item.row.title
        is AndroidBrowseItem.Album -> item.row.title
        is AndroidBrowseItem.Artist -> item.row.name
        is AndroidBrowseItem.Genre -> item.row.name
        is AndroidBrowseItem.HomeGenre -> item.row.name
        is AndroidBrowseItem.Mood -> item.row.name
        is AndroidBrowseItem.Playlist -> item.row.name
        is AndroidBrowseItem.SmartPlaylist -> item.row.name
        is AndroidBrowseItem.Recent -> item.row.title
        is AndroidBrowseItem.Folder -> item.title
    }

internal val AndroidBrowseRow.artist: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.artist
        is AndroidBrowseItem.History -> item.row.artist
        is AndroidBrowseItem.PlaylistEntry -> item.row.artist
        is AndroidBrowseItem.Queued -> item.row.artist
        is AndroidBrowseItem.Album -> item.row.displayArtist
        is AndroidBrowseItem.Artist -> item.row.name
        else -> ""
    }

internal val AndroidBrowseRow.album: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.album
        is AndroidBrowseItem.History -> item.row.album
        is AndroidBrowseItem.PlaylistEntry -> item.row.album
        is AndroidBrowseItem.Queued -> item.row.album
        is AndroidBrowseItem.Album -> item.row.title
        else -> ""
    }

internal val AndroidBrowseRow.subtitle: String
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> if (displayTitle != null) "${item.row.artist}\n${item.row.album}" else "${item.row.artist} · ${item.row.album}"
        is AndroidBrowseItem.History -> "${item.row.artist} · ${item.row.album}"
        is AndroidBrowseItem.PlaylistEntry -> "${item.row.artist} · ${item.row.album}"
        is AndroidBrowseItem.Queued -> "${item.row.artist} · ${item.row.album}"
        is AndroidBrowseItem.Album -> item.row.displayArtist
        is AndroidBrowseItem.Recent -> item.row.subtitle
        else -> ""
    }

internal val AndroidBrowseRow.favorite: Boolean
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.favorite
        is AndroidBrowseItem.History -> item.row.favorite
        is AndroidBrowseItem.PlaylistEntry -> item.row.favorite
        is AndroidBrowseItem.Album -> item.row.favorite
        is AndroidBrowseItem.Artist -> item.row.favorite
        is AndroidBrowseItem.Queued -> item.favorite
        else -> false
    }

internal val AndroidBrowseRow.durationMillis: ULong
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.History -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.PlaylistEntry -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Album -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Artist -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Genre -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Mood -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Playlist -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.SmartPlaylist -> item.row.durationMillis.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Queued -> item.row.durationMillis.coerceAtLeast(0).toULong()
        else -> 0UL
    }

internal val AndroidBrowseRow.detailRoute: Route?
    get() = when (val item = item) {
        is AndroidBrowseItem.Album -> Route.AlbumDetail(item.row.mediaUri)
        is AndroidBrowseItem.Artist -> if (item.albumArtist) Route.AlbumArtistDetail(item.row.mediaUri) else Route.ArtistDetail(item.row.mediaUri)
        is AndroidBrowseItem.Genre -> Route.GenreDetail(item.row.genreKey)
        is AndroidBrowseItem.HomeGenre -> Route.GenreDetail(item.row.genreKey)
        is AndroidBrowseItem.Mood -> Route.MoodDetail(item.row.moodKey)
        is AndroidBrowseItem.Playlist -> Route.PlaylistDetail(item.row.playlistKey)
        is AndroidBrowseItem.SmartPlaylist -> Route.SmartPlaylistDetail(item.row.smartPlaylistKey)
        is AndroidBrowseItem.Folder -> item.route
        is AndroidBrowseItem.Recent -> when (item.row.kind) {
            RecentSearchKind.ALBUM -> Route.AlbumDetail(item.row.mediaUri)
            RecentSearchKind.ARTIST -> Route.ArtistDetail(item.row.mediaUri)
            RecentSearchKind.TRACK -> null
        }
        else -> null
    }

internal val AndroidBrowseRow.playbackContextId: String?
    get() = when (val item = item) {
        is AndroidBrowseItem.Album -> "album:${item.row.mediaUri}"
        is AndroidBrowseItem.Artist -> "${if (item.albumArtist) "album-artist" else "artist"}:${item.row.mediaUri}"
        is AndroidBrowseItem.Genre -> "genre:${item.row.genreKey}"
        is AndroidBrowseItem.HomeGenre -> "genre:${item.row.genreKey}"
        is AndroidBrowseItem.Mood -> "mood:${item.row.moodKey}"
        is AndroidBrowseItem.Playlist -> "playlist:${item.row.playlistKey}"
        is AndroidBrowseItem.SmartPlaylist -> "smart-playlist:${item.row.smartPlaylistKey}"
        is AndroidBrowseItem.Recent -> if (item.row.kind == RecentSearchKind.TRACK) null else "${item.row.kind.name.lowercase()}:${item.row.mediaUri}"
        else -> null
    }

internal val AndroidBrowseRow.year: Long?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.year
        is AndroidBrowseItem.History -> item.row.year
        is AndroidBrowseItem.PlaylistEntry -> item.row.year
        is AndroidBrowseItem.Album -> item.row.year
        is AndroidBrowseItem.Queued -> item.row.year
        else -> null
    }

internal val AndroidBrowseRow.trackCount: ULong
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> 1UL
        is AndroidBrowseItem.History -> 1UL
        is AndroidBrowseItem.PlaylistEntry -> 1UL
        is AndroidBrowseItem.Queued -> 1UL
        is AndroidBrowseItem.Album -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Artist -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Genre -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.HomeGenre -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Mood -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.Playlist -> item.row.trackCount.coerceAtLeast(0).toULong()
        is AndroidBrowseItem.SmartPlaylist -> item.row.trackCount.coerceAtLeast(0).toULong()
        else -> 0UL
    }

internal val AndroidBrowseRow.artworkIdentity: ByteArray?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.artworkBinding
        is AndroidBrowseItem.History -> item.row.artworkBinding
        is AndroidBrowseItem.PlaylistEntry -> item.row.artworkBinding
        is AndroidBrowseItem.Album -> item.row.artworkBinding
        is AndroidBrowseItem.Artist -> item.row.artworkBinding
        is AndroidBrowseItem.HomeGenre -> item.row.artworkBinding
        is AndroidBrowseItem.Recent -> item.row.artworkBinding
        is AndroidBrowseItem.Queued -> item.row.artworkBinding
        is AndroidBrowseItem.Genre -> item.row.artworkBinding ?: item.row.representativeArtwork.firstOrNull()
        is AndroidBrowseItem.Mood -> item.row.representativeArtwork.firstOrNull()
        is AndroidBrowseItem.Playlist -> item.row.artworkBinding ?: item.row.representativeArtwork.firstOrNull()
        is AndroidBrowseItem.SmartPlaylist -> item.row.artworkBinding ?: item.row.artworkBindings.firstOrNull()
        is AndroidBrowseItem.Folder -> item.artwork
    }

internal val AndroidBrowseRow.downloaded: Boolean
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.isDownloaded
        is AndroidBrowseItem.History -> item.row.isDownloaded
        is AndroidBrowseItem.PlaylistEntry -> item.row.isDownloaded
        is AndroidBrowseItem.Album -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.Artist -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.Genre -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.HomeGenre -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.Mood -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.Playlist -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.SmartPlaylist -> item.row.trackCount > 0 && item.row.downloadedCount == item.row.trackCount
        is AndroidBrowseItem.Queued -> item.downloaded
        else -> false
    }

internal val AndroidBrowseRow.sourceId: String?
    get() = contextSourceId ?: when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.sourceId
        is AndroidBrowseItem.PlaylistEntry -> item.row.sourceId
        is AndroidBrowseItem.Genre -> item.row.sourceId
        is AndroidBrowseItem.Playlist -> item.row.sourceId
        is AndroidBrowseItem.Album -> sourceIdForUri(item.row.mediaUri)
        is AndroidBrowseItem.Artist -> sourceIdForUri(item.row.mediaUri)
        is AndroidBrowseItem.History -> sourceIdForUri(item.row.mediaUri)
        is AndroidBrowseItem.Recent -> sourceIdForUri(item.row.mediaUri)
        is AndroidBrowseItem.Queued -> sourceIdForUri(item.row.mediaUri)
        else -> null
    }

internal val AndroidBrowseRow.sourceName: String
    get() = contextSourceName.takeIf { it.isNotEmpty() } ?: when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.sourceName
        is AndroidBrowseItem.History -> item.row.sourceName.orEmpty()
        is AndroidBrowseItem.PlaylistEntry -> item.row.sourceName.orEmpty()
        else -> contextSourceName
    }

internal val AndroidBrowseRow.lastPlayed: Long?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.lastPlayed
        is AndroidBrowseItem.History -> item.row.lastPlayed
        is AndroidBrowseItem.PlaylistEntry -> item.row.lastPlayed
        is AndroidBrowseItem.Album -> item.row.lastPlayed
        is AndroidBrowseItem.Artist -> item.row.lastPlayed
        else -> null
    }

internal val AndroidBrowseRow.writable: Boolean
    get() = when (val item = item) {
        is AndroidBrowseItem.Playlist -> item.row.writable
        else -> false
    }

internal val AndroidBrowseRow.trackKey: Long?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.trackKey
        is AndroidBrowseItem.History -> item.row.trackKey
        is AndroidBrowseItem.PlaylistEntry -> item.row.trackKey
        else -> null
    }

internal val AndroidBrowseRow.albumKey: Long?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.albumKey
        else -> null
    }

internal val AndroidBrowseRow.albumMediaUri: String?
    get() = when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.albumMediaUri
        else -> null
    }

internal val Route.kind: String
    get() = when (this) {
        is Route.Home -> "Home"
        is Route.Search -> "Search"
        is Route.Favorites -> "Favorites"
        is Route.History -> "History"
        is Route.Albums -> "Albums"
        is Route.AlbumDetail -> "AlbumDetail"
        is Route.Tracks -> "Tracks"
        is Route.Artists -> "Artists"
        is Route.ArtistDetail -> "ArtistDetail"
        is Route.ArtistDiscography -> "ArtistDiscography"
        is Route.ArtistTracks -> "ArtistTracks"
        is Route.ArtistFavoriteTracks -> "ArtistFavoriteTracks"
        is Route.AlbumArtists -> "AlbumArtists"
        is Route.AlbumArtistDetail -> "AlbumArtistDetail"
        is Route.AlbumArtistDiscography -> "AlbumArtistDiscography"
        is Route.AlbumArtistTracks -> "AlbumArtistTracks"
        is Route.AlbumArtistFavoriteTracks -> "AlbumArtistFavoriteTracks"
        is Route.Genres -> "Genres"
        is Route.GenreDetail -> "GenreDetail"
        is Route.Moods -> "Moods"
        is Route.MoodDetail -> "MoodDetail"
        is Route.Folders -> "Folders"
        is Route.Playlists -> "Playlists"
        is Route.PlaylistDetail -> "PlaylistDetail"
        is Route.SmartPlaylists -> "SmartPlaylists"
        is Route.SmartPlaylistDetail -> "SmartPlaylistDetail"
    }

internal fun AndroidBrowseRow.withFavorite(value: Boolean): AndroidBrowseRow = copy(item = when (val item = item) {
    is AndroidBrowseItem.Track -> item.copy(row = item.row.copy(favorite = value))
    is AndroidBrowseItem.History -> item.copy(row = item.row.copy(favorite = value))
    is AndroidBrowseItem.PlaylistEntry -> item.copy(row = item.row.copy(favorite = value))
    is AndroidBrowseItem.Album -> item.copy(row = item.row.copy(favorite = value))
    is AndroidBrowseItem.Artist -> item.copy(row = item.row.copy(favorite = value))
    is AndroidBrowseItem.Queued -> item.copy(favorite = value)
    else -> item
})

internal fun favoriteTarget(kind: String, uri: String): io.github.screwys.rufin.shared.library.FavoriteTarget = when (kind) {
    "track" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Track(uri)
    "album", "album_header" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Album(uri)
    "artist" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Artist(uri)
    else -> error("This item has no favorite action")
}
internal val io.github.screwys.rufin.shared.rufin_core.FavoriteSettlement.kind: String
    get() = when (target) {
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Track -> "track"
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Album -> "album"
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Artist -> "artist"
    }
internal val io.github.screwys.rufin.shared.rufin_core.FavoriteSettlement.mediaUri: String
    get() = when (val item = target) {
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Track -> item.v1
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Album -> item.v1
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Artist -> item.v1
    }

internal val AndroidBrowseRow.rating: UByte?
    get() = (when (val item = item) {
        is AndroidBrowseItem.Track -> item.row.rating
        is AndroidBrowseItem.History -> item.row.rating
        is AndroidBrowseItem.PlaylistEntry -> item.row.rating
        is AndroidBrowseItem.Album -> item.row.rating
        is AndroidBrowseItem.Artist -> item.row.rating
        else -> null
    })?.takeIf { it in 0..255 }?.toUByte()
