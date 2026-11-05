package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.clockText
import io.github.screwys.rufin.core.AndroidBrowseItem
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.shared.library.AlbumRow
import io.github.screwys.rufin.shared.library.ArtistRow
import io.github.screwys.rufin.shared.library.PlaylistFolderRow
import io.github.screwys.rufin.shared.library.PlaylistListRow
import io.github.screwys.rufin.shared.library.PlaylistRow
import io.github.screwys.rufin.shared.library.Row
import io.github.screwys.rufin.shared.library.SmartPlaylistRow
import io.github.screwys.rufin.shared.library.FavoriteTarget
import io.github.screwys.rufin.shared.rufin_core.MetadataItemId
import io.github.screwys.rufin.shared.rufin_core.RecentSearchKind
import io.github.screwys.rufin.shared.library.TagKind
import io.github.screwys.rufin.shared.library.TagRow
import io.github.screwys.rufin.shared.library.TrackRow
import io.github.screwys.rufin.shared.library.sourceIdForUri
import io.github.screwys.rufin.shared.rufin_core.Route

/** An artist page that lists the artist's releases. */
internal val Route.showsReleases: Boolean
    get() = this is Route.ArtistDetail || this is Route.AlbumArtistDetail ||
        this is Route.ArtistDiscography || this is Route.AlbumArtistDiscography

internal val AndroidBrowseRow.libraryRow: Row? get() = (item as? AndroidBrowseItem.Library)?.row
internal val AndroidBrowseRow.recent get() = (item as? AndroidBrowseItem.Recent)?.row
private val AndroidBrowseRow.folder get() = (route as? Route.Folders)?.path?.lastOrNull()?.takeIf { item is AndroidBrowseItem.Folder }
internal val AndroidBrowseRow.track: TrackRow?
    get() = when (val row = libraryRow) {
        is Row.Track -> row.v1
        is Row.AlbumTrack -> row.track
        else -> null
    }
internal val AndroidBrowseRow.albumRow: AlbumRow?
    get() = when (val row = libraryRow) {
        is Row.Album -> row.v1
        is Row.AlbumHeading -> row.v1
        else -> null
    }
internal val AndroidBrowseRow.artistRow: ArtistRow? get() = (libraryRow as? Row.Artist)?.v1
internal val AndroidBrowseRow.tag: TagRow? get() = (libraryRow as? Row.Tag)?.v1
internal val AndroidBrowseRow.genre: TagRow? get() = tag?.takeIf { it.kind == TagKind.GENRE }
private val AndroidBrowseRow.playlistListRow: PlaylistListRow? get() = (libraryRow as? Row.Playlist)?.v1
internal val AndroidBrowseRow.playlist: PlaylistRow? get() = (playlistListRow as? PlaylistListRow.Playlist)?.v1
internal val AndroidBrowseRow.smartPlaylist: SmartPlaylistRow? get() = (playlistListRow as? PlaylistListRow.SmartPlaylist)?.v1
internal val AndroidBrowseRow.playlistFolder: PlaylistFolderRow? get() = (playlistListRow as? PlaylistListRow.Folder)?.v1
internal val AndroidBrowseRow.isAlbumTrack: Boolean get() = libraryRow is Row.AlbumTrack

/** The track, album or artist the row shows, which it favorites, rates and edits. */
internal val AndroidBrowseRow.target: FavoriteTarget?
    get() = when (val item = item) {
        is AndroidBrowseItem.Library -> when (val row = item.row) {
            is Row.Track -> FavoriteTarget.Track(row.v1.mediaUri)
            is Row.AlbumTrack -> FavoriteTarget.Track(row.track.mediaUri)
            is Row.Album -> FavoriteTarget.Album(row.v1.mediaUri)
            is Row.AlbumHeading -> FavoriteTarget.Album(row.v1.mediaUri)
            is Row.Artist -> FavoriteTarget.Artist(row.v1.mediaUri)
            else -> null
        }
        is AndroidBrowseItem.Recent -> when (item.row.kind) {
            RecentSearchKind.TRACK -> FavoriteTarget.Track(item.row.mediaUri)
            RecentSearchKind.ALBUM -> FavoriteTarget.Album(item.row.mediaUri)
            RecentSearchKind.ARTIST -> FavoriteTarget.Artist(item.row.mediaUri)
        }
        is AndroidBrowseItem.Folder -> null
    }

internal val AndroidBrowseRow.isTrack: Boolean get() = target is FavoriteTarget.Track
internal val AndroidBrowseRow.isFolder: Boolean get() = item is AndroidBrowseItem.Folder

/** The editor of the row's track, album or artist. */
internal val AndroidBrowseRow.metadataItem: MetadataItemId?
    get() = when (val target = target) {
        is FavoriteTarget.Track -> MetadataItemId.Track(target.v1)
        is FavoriteTarget.Album -> MetadataItemId.Album(target.v1)
        is FavoriteTarget.Artist -> MetadataItemId.Artist(target.v1)
        null -> null
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
        ?: playlistFolder?.let { "${io.github.screwys.rufin.core.trackCountText(trackCount)} · ${clockText(durationMillis)}" }
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

internal val AndroidBrowseRow.sourceId: String?
    get() = track?.sourceId ?: genre?.sourceId ?: playlist?.sourceId
        ?: (albumRow?.mediaUri ?: artistRow?.mediaUri ?: recent?.mediaUri)?.let(::sourceIdForUri)

internal val AndroidBrowseRow.rating: UByte?
    get() = (track?.state ?: albumRow?.state ?: artistRow?.state)?.rating?.takeIf { it in 0..255 }?.toUByte()

internal val AndroidBrowseRow.playlistFolderMember: io.github.screwys.rufin.shared.library.PlaylistFolderMember?
    get() = playlist?.let { io.github.screwys.rufin.shared.library.PlaylistFolderMember.Playlist(it.playlistKey) }
        ?: smartPlaylist?.let { io.github.screwys.rufin.shared.library.PlaylistFolderMember.SmartPlaylist(it.smartPlaylistKey) }
