package io.github.screwys.rufin.browse

import io.github.screwys.rufin.core.AndroidBrowseItem
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.core.AndroidDetailSummary
import io.github.screwys.rufin.shared.library.AlbumRow
import io.github.screwys.rufin.shared.library.ArtistRow
import io.github.screwys.rufin.shared.library.PlaylistFolderRow
import io.github.screwys.rufin.shared.library.PlaylistListRow
import io.github.screwys.rufin.shared.library.PlaylistRow
import io.github.screwys.rufin.shared.library.Row
import io.github.screwys.rufin.shared.library.SmartPlaylistRow
import io.github.screwys.rufin.shared.library.StateChange
import io.github.screwys.rufin.shared.library.TagKind
import io.github.screwys.rufin.shared.library.TagRow
import io.github.screwys.rufin.shared.library.TrackRow
import io.github.screwys.rufin.shared.library.sourceIdForUri
import io.github.screwys.rufin.shared.rufin_core.Route

internal val AndroidDetailSummary.showsReleases: Boolean
    get() = mode.kind in listOf("ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography")

private val AndroidBrowseRow.libraryRow: Row? get() = (item as? AndroidBrowseItem.Library)?.row
private val AndroidBrowseRow.recent get() = (item as? AndroidBrowseItem.Recent)?.row
private val AndroidBrowseRow.folder get() = (route as? Route.Folders)?.path?.lastOrNull()?.takeIf { item is AndroidBrowseItem.Folder }
internal val AndroidBrowseRow.track: TrackRow?
    get() = when (val row = libraryRow) {
        is Row.Track -> row.v1
        is Row.AlbumTrack -> row.track
        else -> null
    }
private val AndroidBrowseRow.albumRow: AlbumRow?
    get() = when (val row = libraryRow) {
        is Row.Album -> row.v1
        is Row.AlbumHeading -> row.v1
        else -> null
    }
private val AndroidBrowseRow.artistRow: ArtistRow? get() = (libraryRow as? Row.Artist)?.v1
private val AndroidBrowseRow.tag: TagRow? get() = (libraryRow as? Row.Tag)?.v1
private val AndroidBrowseRow.genre: TagRow? get() = tag?.takeIf { it.kind == TagKind.GENRE }
private val AndroidBrowseRow.playlistListRow: PlaylistListRow? get() = (libraryRow as? Row.Playlist)?.v1
internal val AndroidBrowseRow.playlist: PlaylistRow? get() = (playlistListRow as? PlaylistListRow.Playlist)?.v1
internal val AndroidBrowseRow.smartPlaylist: SmartPlaylistRow? get() = (playlistListRow as? PlaylistListRow.SmartPlaylist)?.v1
internal val AndroidBrowseRow.playlistFolder: PlaylistFolderRow? get() = (playlistListRow as? PlaylistListRow.Folder)?.v1
internal val AndroidBrowseRow.isAlbumTrack: Boolean get() = libraryRow is Row.AlbumTrack

internal val AndroidBrowseRow.kind: String
    get() = when {
        track != null -> "track"
        albumRow != null -> if (libraryRow is Row.AlbumHeading) "album_header" else "album"
        artistRow != null -> "artist"
        tag != null -> if (genre != null) "genre" else "mood"
        playlist != null -> "playlist"
        smartPlaylist != null -> "smart_playlist"
        playlistFolder != null -> "playlist_folder"
        else -> recent?.kind?.name?.lowercase() ?: "folder"
    }

internal val AndroidBrowseRow.key: String
    get() = track?.let { it.entry?.key?.toString() ?: it.mediaUri } ?: albumRow?.objectId ?: artistRow?.objectId
        ?: genre?.objectId ?: tag?.key?.toString() ?: playlist?.objectId ?: smartPlaylist?.objectId
        ?: playlistFolder?.let { "playlist-folder:${it.objectId}" } ?: recent?.mediaUri ?: folder?.id.orEmpty()

internal val AndroidBrowseRow.mediaUri: String
    get() = track?.mediaUri ?: albumRow?.mediaUri ?: artistRow?.mediaUri ?: recent?.mediaUri ?: ""

internal val AndroidBrowseRow.title: String
    get() = displayTitle ?: track?.title ?: albumRow?.title ?: artistRow?.name ?: tag?.name ?: playlist?.name
        ?: smartPlaylist?.name ?: playlistFolder?.name ?: recent?.title ?: folder?.name.orEmpty()

internal val AndroidBrowseRow.artist: String
    get() = track?.artist ?: albumRow?.artist ?: artistRow?.name ?: ""

internal val AndroidBrowseRow.album: String
    get() = track?.album ?: albumRow?.title ?: ""

internal val AndroidBrowseRow.subtitle: String
    get() = track?.let { "${it.artist} · ${it.album}" }
        ?: albumRow?.artist
        ?: playlistFolder?.let { "${io.github.screwys.rufin.core.trackCountText(trackCount)} · ${browseDuration(durationMillis)}" }
        ?: recent?.subtitle ?: ""

/** The key of the track, album or artist whose favorite and rating the row shows. */
internal val AndroidBrowseRow.stateKey: Long?
    get() = track?.trackKey ?: albumRow?.albumKey ?: artistRow?.artistKey

internal val AndroidBrowseRow.favorite: Boolean
    get() = (track?.state ?: albumRow?.state ?: artistRow?.state)?.favorite == true

private val AndroidBrowseRow.summary get() = albumRow?.summary ?: artistRow?.summary ?: tag?.summary

internal val AndroidBrowseRow.durationMillis: ULong
    get() = (track?.durationMillis ?: summary?.durationMillis ?: playlist?.durationMillis ?: smartPlaylist?.durationMillis
        ?: playlistFolder?.durationMillis ?: 0L).coerceAtLeast(0).toULong()

internal val AndroidBrowseRow.year: Long?
    get() = track?.year ?: albumRow?.year

internal val AndroidBrowseRow.trackCount: ULong
    get() = if (track != null) 1UL else (summary?.trackCount ?: playlist?.trackCount ?: smartPlaylist?.trackCount
        ?: playlistFolder?.trackCount ?: 0L).coerceAtLeast(0).toULong()

internal val AndroidBrowseRow.artworkIdentity: ByteArray?
    get() = when (val item = item) {
        is AndroidBrowseItem.Folder -> item.artwork
        is AndroidBrowseItem.Recent -> item.row.artworkBinding
        is AndroidBrowseItem.Library -> track?.artworkBinding ?: albumRow?.artworkBinding ?: artistRow?.artworkBinding
            ?: genre?.let { it.artworkBinding ?: it.representativeArtwork.firstOrNull() }
            ?: tag?.representativeArtwork?.firstOrNull()
            ?: playlist?.artworkBinding ?: smartPlaylist?.artworkBinding ?: playlistFolder?.artworkBinding
    }

internal val AndroidBrowseRow.downloaded: Boolean
    get() = track?.isDownloaded ?: summary?.let { it.trackCount > 0 && it.downloadedCount == it.trackCount }
        ?: playlist?.let { it.trackCount > 0 && it.downloadedCount == it.trackCount }
        ?: smartPlaylist?.let { it.trackCount > 0 && it.downloadedCount == it.trackCount } ?: false

internal val AndroidBrowseRow.sourceId: String?
    get() = track?.sourceId ?: genre?.sourceId ?: playlist?.sourceId
        ?: (albumRow?.mediaUri ?: artistRow?.mediaUri ?: recent?.mediaUri)?.let(::sourceIdForUri)

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
        is Route.PlaylistFolder -> "PlaylistFolder"
        is Route.SmartPlaylistDetail -> "SmartPlaylistDetail"
    }

internal fun favoriteTarget(kind: String, uri: String): io.github.screwys.rufin.shared.library.FavoriteTarget = when (kind) {
    "track" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Track(uri)
    "album", "album_header" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Album(uri)
    "artist" -> io.github.screwys.rufin.shared.library.FavoriteTarget.Artist(uri)
    else -> error("This item has no favorite action")
}
internal val StateChange.kind: String
    get() = when (target) {
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Track -> "track"
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Album -> "album"
        is io.github.screwys.rufin.shared.library.FavoriteTarget.Artist -> "artist"
    }

internal val AndroidBrowseRow.rating: UByte?
    get() = (track?.state ?: albumRow?.state ?: artistRow?.state)?.rating?.takeIf { it in 0..255 }?.toUByte()

internal val AndroidBrowseRow.playlistFolderMember: io.github.screwys.rufin.shared.library.PlaylistFolderMember?
    get() = playlist?.let { io.github.screwys.rufin.shared.library.PlaylistFolderMember.Playlist(it.playlistKey) }
        ?: smartPlaylist?.let { io.github.screwys.rufin.shared.library.PlaylistFolderMember.SmartPlaylist(it.smartPlaylistKey) }
