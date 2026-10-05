package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.DetailLinkIcon
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.FallbackCover
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.player.PlayerConnection

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.paging.LoadState
import androidx.paging.compose.LazyPagingItems
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.core.AndroidDetailLink
import io.github.screwys.rufin.core.albumCountText
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.core.AndroidDetailSummary
import io.github.screwys.rufin.core.AndroidArtistReleaseGroup
import io.github.screwys.rufin.core.AndroidSortField
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.flow.distinctUntilChanged

@Composable
internal fun DetailContent(
    summary: AndroidDetailSummary?, page: BrowsePage, player: PlayerConnection,
    favoritesPage: BrowsePage?, releasePages: List<Pair<AndroidArtistReleaseGroup, BrowsePage>>,
    onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    onPlay: (BrowseItem, String, Boolean) -> Unit, onFavorite: (BrowseItem) -> Unit,
    onAddToPlaylist: (BrowseItem) -> Unit, onOpenRoute: (String, String) -> Unit,
    onLink: (AndroidDetailLink) -> Unit, onScrolled: (Boolean) -> Unit, onRadio: (BrowseItem) -> Unit,
) {
    val tracks = page.pages.collectAsLazyPagingItems()
    val favorites = favoritesPage?.pages?.collectAsLazyPagingItems()
    val releases = releasePages.filter { it.first.total > 0UL }.map { (group, owner) ->
        Triple(group, owner, key(group.id) { owner.pages.collectAsLazyPagingItems() })
    }
    val state = rememberLazyListState()
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
            .distinctUntilChanged().collect(onScrolled)
    }
    if (summary == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            if (page.loading) CircularProgressIndicator() else Text(translate("This isn't available"))
        }
        return
    }
    val headerItem = remember(summary, page.displaySettings) { BrowseItem(summary.row, 0UL, null, page.displaySettings) }
    val artist = summary.mode == "ArtistDetail" || summary.mode == "AlbumArtistDetail"
    val releasesVisible = artist || summary.mode == "ArtistDiscography" || summary.mode == "AlbumArtistDiscography"
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val width = maxWidth
        LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
            item("detail-header") {
                DetailHeader(summary, headerItem, player, onPlay, onFavorite, onOpenRoute, onLink, onMenu, onRadio)
            }
            if (artist && favorites != null && favoritesPage != null && summary.favoriteTotal > 0UL) item("detail-favorites") {
                Column {
                    DetailSectionHeading(translate("Favorite tracks"))
                    if (summary.showTrackHeader) DetailTrackHeader(favoritesPage.detailTrackFields, favoritesPage.displaySettings?.size)
                    val visibleRows = if (favorites.loadState.refresh is LoadState.Loading)
                        minOf(summary.favoriteTotal, 4UL).toInt() else minOf(favorites.itemCount, 4)
                    val baseHeight = when (favoritesPage.displaySettings?.size) { "Compact" -> 48.dp; "Large" -> 80.dp; else -> 64.dp }
                    val rowHeight = baseHeight * LocalDensity.current.fontScale.coerceAtLeast(1f)
                    if (favorites.itemCount == 0 && favorites.loadState.refresh is LoadState.NotLoading) {
                        BrowseEmptyState(favoritesPage.hasActiveFilters, resetFilters = page::resetFilters)
                    } else LazyColumn(Modifier.fillMaxWidth().height(rowHeight * visibleRows.coerceAtLeast(1))) {
                        items(favorites.itemCount, key = favorites.itemKey { "${it.row.key}:${it.index}" }) { index ->
                            val item = favorites[index]
                            if (item == null) Spacer(Modifier.height(64.dp)) else DetailTrackRow(item.copy(displaySettings = favoritesPage.options.displaySettings ?: favoritesPage.displaySettings), player,
                                summary.showTrackImages, favoritesPage.detailTrackFields, onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                        }
                        item { DetailLoadState(favorites.loadState.append, favorites::retry) }
                    }
                    DetailLoadState(favorites.loadState.refresh, favorites::retry)
                }
            }
            if (releasesVisible) {
                releases.forEach { (group, owner, albums) ->
                    val display = owner.displaySettings
                    val grid = display?.layout == "Grid"
                    val cardSize = when (display?.size) { "Compact" -> 104.dp; "Large" -> 176.dp; else -> 128.dp }
                    val gap = when (display?.gridSpacing) { "Small" -> 4.dp; "Wide" -> 16.dp; else -> 12.dp }
                    val columns = if (grid) ((width - 32.dp + gap) / (cardSize + gap)).toInt().coerceAtLeast(1) else 1
                    val count = albums.itemCount
                    item("release-heading:${group.id}") { DetailSectionHeading(group.title) }
                    items((count + columns - 1) / columns, key = { "release:${group.id}:$it" }) { rowIndex ->
                            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(gap)) {
                                repeat(columns) { column ->
                                    val position = rowIndex * columns + column
                                    if (position >= count) Spacer(Modifier.weight(1f)) else {
                                        val item = albums[position]
                                        Box(Modifier.weight(1f)) {
                                            if (item == null) Spacer(Modifier.height(if (grid) cardSize + 64.dp else 76.dp))
                                            else if (grid) CoverCard(item.copy(displaySettings = owner.options.displaySettings ?: owner.displaySettings), player, Modifier.fillMaxWidth().padding(bottom = 12.dp), onActivate, onMenu)
                                            else DetailAlbumRow(item.copy(displaySettings = owner.options.displaySettings ?: owner.displaySettings), player, onActivate, onMenu, onFavorite)
                                        }
                                    }
                                }
                            }
                        }
                    item("release-state:${group.id}") {
                        DetailLoadState(albums.loadState.refresh, albums::retry)
                        DetailLoadState(albums.loadState.append, albums::retry)
                        if (count == 0 && albums.loadState.refresh is LoadState.NotLoading)
                            BrowseEmptyState(owner.hasActiveFilters, resetFilters = page::resetFilters)
                    }
                }
            } else {
                val display = page.options.displaySettings ?: page.displaySettings
                if (display?.layout == "Grid") {
                    val cardSize = when (display.size) { "Compact" -> 104.dp; "Large" -> 176.dp; else -> 128.dp }
                    val gap = when (display.gridSpacing) { "Small" -> 4.dp; "Wide" -> 16.dp; else -> 12.dp }
                    val columns = ((width - 32.dp + gap) / (cardSize + gap)).toInt().coerceAtLeast(1)
                    items((tracks.itemCount + columns - 1) / columns, key = { "detail-grid:$it" }) { rowIndex ->
                        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(gap)) {
                            repeat(columns) { column ->
                                val position = rowIndex * columns + column
                                if (position >= tracks.itemCount) Spacer(Modifier.weight(1f)) else Box(Modifier.weight(1f)) {
                                    val item = tracks[position]
                                    if (item == null) Spacer(Modifier.height(cardSize + 48.dp))
                                    else CoverCard(item.copy(displaySettings = display), player, Modifier.fillMaxWidth(), onActivate, onMenu)
                                }
                            }
                        }
                    }
                } else {
                    if (summary.showTrackHeader) item("detail-track-header") { DetailTrackHeader(page.detailTrackFields, display?.size) }
                    items(tracks.itemCount, key = tracks.itemKey { "${it.row.key}:${it.index}" }) { index ->
                        val item = tracks[index]
                        if (item == null) Spacer(Modifier.height(48.dp))
                        else DetailTrackRow(item.copy(displaySettings = display), player,
                            summary.showTrackImages, page.detailTrackFields, onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                    }
                }
                item { DetailLoadState(tracks.loadState.refresh, tracks::retry); DetailLoadState(tracks.loadState.append, tracks::retry) }
                if (tracks.itemCount == 0 && tracks.loadState.refresh is LoadState.NotLoading) item {
                    BrowseEmptyState(page.hasActiveFilters, resetFilters = page::resetFilters)
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class, ExperimentalFoundationApi::class)
@Composable
private fun DetailHeader(
    summary: AndroidDetailSummary, item: BrowseItem, player: PlayerConnection,
    onPlay: (BrowseItem, String, Boolean) -> Unit, onFavorite: (BrowseItem) -> Unit,
    onOpenRoute: (String, String) -> Unit, onLink: (AndroidDetailLink) -> Unit, onMenu: (BrowseItem) -> Unit,
    onRadio: (BrowseItem) -> Unit,
) {
    val projected = favoriteItem(player.model, item)
    if (summary.mode in setOf("ArtistDiscography", "AlbumArtistDiscography", "ArtistTracks", "AlbumArtistTracks", "ArtistFavoriteTracks", "AlbumArtistFavoriteTracks")) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).then(showcasePaint(item.row.key)).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(translate(when {
                summary.mode.endsWith("Discography") -> "Discography"
                summary.mode.endsWith("FavoriteTracks") -> "Favorite tracks"
                else -> "Tracks"
            }).uppercase(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
            BrowseTitle(item.row, maxLines = 1, style = MaterialTheme.typography.titleLarge, marquee = true, player = player)
            Text("${albumCountText(summary.albumCount)} / ${trackCountText(item.row.trackCount)}",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        return
    }
    BoxWithConstraints(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
        val coverSize = ((maxWidth - 24.dp) * .35f).coerceIn(1.dp, 224.dp)
        Row(Modifier.fillMaxWidth().then(showcasePaint(item.row.key)).padding(12.dp),
            verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(18.dp)) {
            Column(Modifier.width(coverSize), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Box(Modifier.size(coverSize).pointerInput(item.row.key) { detectTapGestures(onLongPress = { onMenu(item) }) }) {
                    DetailHeaderCover(summary, player, coverSize)
                }
                if (summary.links.isNotEmpty() && item.row.kind !in setOf("playlist", "smart_playlist")) CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                    FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                        summary.links.forEach { link ->
                            IconButton({ onLink(link) }, Modifier.size(32.dp).semantics { contentDescription = link.title }) {
                                DetailLinkIcon(link.iconId, Modifier.size(20.dp))
                            }
                        }
                    }
                }
            }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(summary.label.uppercase(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                    if (item.row.kind in setOf("album", "artist", "genre")) CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                        PlayerIconButton("rufin-audio-only-symbolic", "Play radio", { onRadio(item) }, Modifier.size(32.dp), iconSize = 16.dp)
                    }
                }
                BrowseTitle(item.row, maxLines = 1, style = MaterialTheme.typography.titleLarge, marquee = true, player = player)
                if (item.row.kind == "album" && summary.artistText.isNotBlank()) {
                    val linkColor = MaterialTheme.colorScheme.onSurface
                    val credited = buildAnnotatedString {
                        append(summary.artistText)
                        summary.artistLinks.forEach { artist ->
                            addLink(LinkAnnotation.Clickable(artist.route,
                                TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.None)),
                                { onOpenRoute(artist.route, artist.title) }), artist.start.toInt(), artist.end.toInt())
                        }
                    }
                    Text(credited, Modifier.rufinMarquee(), maxLines = 1, style = MaterialTheme.typography.bodyMedium, color = linkColor)
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (item.row.kind == "artist") {
                        DetailFact("rufin-albums-symbolic", albumCountText(summary.albumCount), "Albums") {
                            summary.discographyRoute?.let { onOpenRoute(it, translate("Albums")) }
                        }
                        DetailFact("rufin-tracks-symbolic", trackCountText(item.row.trackCount), "Tracks") {
                            summary.tracksRoute?.let { onOpenRoute(it, translate("Tracks")) }
                        }
                    } else {
                        item.row.year?.let { DetailFact("rufin-x-office-calendar-symbolic", it.toString(), "Year") }
                        DetailFact("rufin-tracks-symbolic", trackCountText(item.row.trackCount), "Tracks")
                        if (item.row.durationMillis > 0UL || item.row.kind in setOf("playlist", "smart_playlist"))
                            DetailFact("rufin-preferences-system-time-symbolic", detailDuration(item.row.durationMillis), "Duration")
                    }
                }
                if (item.row.kind in setOf("playlist", "smart_playlist")) {
                    player.model.sources?.sources?.find { it.id == item.row.sourceId }?.let { source ->
                        val link = summary.links.firstOrNull()
                        CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                            IconButton({ link?.let(onLink) }, Modifier.size(32.dp), enabled = link?.url != null) {
                                SourceIcon(source.kind, Modifier.size(20.dp).semantics { contentDescription = source.name })
                            }
                        }
                    }
                }
                FlowRow(Modifier.padding(top = 4.dp), horizontalArrangement = Arrangement.spacedBy(4.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    ShowcaseAction("rufin-media-playback-start-symbolic", "Play", { onPlay(item, "now", false) }, { onPlay(item, "now", true) })
                    ShowcaseAction("rufin-mail-forward-symbolic", "Play Next", { onPlay(item, "next", false) }, { onPlay(item, "next", true) })
                    ShowcaseAction("rufin-go-last-symbolic", "Play Later", { onPlay(item, "append", false) }, { onPlay(item, "append", true) })
                    if (item.row.kind in setOf("album", "artist"))
                        ShowcaseAction(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", "Favorite",
                            { onFavorite(projected) }, tint = if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
                }
            }
        }
    }
}

@Composable
private fun DetailHeaderCover(summary: AndroidDetailSummary, player: PlayerConnection, size: Dp) {
    val bindings = summary.artworkBindings
    if (bindings.size <= 1) {
        val row = summary.row.copy(artworkIdentity = bindings.firstOrNull() ?: summary.row.artworkIdentity)
        DetailCover(row, detailArtwork(row, player, size), Modifier.size(size))
    } else {
        Column(Modifier.size(size).clip(RoundedCornerShape(12.dp))) {
            repeat(2) { y ->
                Row {
                    repeat(2) { x ->
                        val row = summary.row.copy(artworkIdentity = bindings[(y * 2 + x) % bindings.size])
                        Artwork(detailArtwork(row, player, size / 2f), Modifier.size(size / 2f))
                    }
                }
            }
        }
    }
}

@Composable
private fun DetailFact(icon: String, text: String, label: String, action: (() -> Unit)? = null) {
    val modifier = (if (action == null) Modifier else Modifier.clickable(onClick = action))
        .semantics { contentDescription = "${translate(label)} $text" }
    Row(modifier, verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        RufinIcon(icon, null, Modifier.size(14.dp))
        Text(text, style = MaterialTheme.typography.labelMedium)
    }
}

@Composable
private fun DetailSectionHeading(title: String) {
    Text(title, Modifier.fillMaxWidth().padding(start = 16.dp, top = 16.dp, bottom = 8.dp),
        style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
}

@Composable
private fun detailArtwork(row: AndroidBrowseRow, player: PlayerConnection, size: Dp): PlayerArtwork? {
    val pixels = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
    return rememberArtwork(row.artworkIdentity, player, pixels)
}

@Composable
private fun DetailCover(row: AndroidBrowseRow, artwork: PlayerArtwork?, modifier: Modifier,
) {
    Box(modifier.clip(RoundedCornerShape(8.dp))) {
        if (artwork == null) FallbackCover(Modifier.fillMaxSize()) else Artwork(artwork, Modifier.fillMaxSize())
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun DetailTrackRow(
    item: BrowseItem, player: PlayerConnection, showImage: Boolean, fields: List<AndroidSortField>,
    activate: (BrowseItem) -> Unit, menu: (BrowseItem) -> Unit,
    favorite: (BrowseItem) -> Unit, playlist: (BrowseItem) -> Unit, allowSwipe: Boolean = true,
) {
    val projected = if (fields.any { it.id == "Favorite" }) favoriteItem(player.model, item) else item
    val size = item.displaySettings?.size
    val merged = fields.any { it.id == "TitleMerged" }
    val imageSize = when (size) { "Compact" -> 36.dp; "Large" -> 64.dp; else -> 48.dp }
    val rowPadding = if (size == "Compact") 0.dp else 4.dp
    val baseHeight = when (size) { "Compact" -> 48.dp; "Large" -> 80.dp; else -> if (merged) 64.dp else 56.dp }
    val rowHeight = baseHeight * LocalDensity.current.fontScale.coerceAtLeast(1f)
    BrowseSwipeActions(item, allowSwipe) {
    Row(Modifier.fillMaxWidth().heightIn(min = rowHeight).combinedClickable(onClick = { activate(item) }, onLongClick = { menu(item) }).padding(start = 8.dp, end = 4.dp, top = rowPadding, bottom = rowPadding),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        fields.forEach { field ->
            val value = item.row.fields.firstOrNull { it.id == field.id }?.text.orEmpty()
            when (field.id) {
                "Image" -> if (showImage) DetailCover(item.row, detailArtwork(item.row, player, imageSize), Modifier.size(imageSize))
                "Tools" -> PlayerIconButton("rufin-view-more-symbolic", "More", { menu(item) })
                "Favorite" -> IconButton({ favorite(projected) }) {
                    RufinIcon(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", translate("Favorite"), Modifier.size(24.dp),
                        if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
                }
                "RowIndex" -> Text(value, detailColumn(field.id), textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodySmall)
                "TitleMerged" -> Row(detailColumn(field.id), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (showImage) DetailCover(item.row, detailArtwork(item.row, player, imageSize), Modifier.size(imageSize))
                    Column(Modifier.weight(1f)) {
                        BrowseTitle(item.row.copy(title = value), maxLines = 1, style = MaterialTheme.typography.bodyMedium, player = player)
                        Text(item.row.artist, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                else -> if (field.id == "Title") BrowseTitle(item.row.copy(title = value), detailColumn(field.id), maxLines = 2,
                    style = MaterialTheme.typography.bodyMedium, player = player)
                else Text(value, detailColumn(field.id), maxLines = 2, overflow = TextOverflow.Ellipsis,
                    style = if (field.id == "Title") MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodySmall)
            }
        }
    }
    }
}

private fun RowScope.detailColumn(id: String, imageSize: Dp = 48.dp): Modifier = when (id) {
    "RowIndex" -> Modifier.width(28.dp)
    "Duration" -> Modifier.width(44.dp)
    "Year" -> Modifier.width(40.dp)
    "Image" -> Modifier.width(imageSize)
    "Tools" -> Modifier.width(48.dp)
    "Title", "TitleMerged" -> Modifier.weight(1f)
    else -> Modifier.weight(.65f)
}

@Composable
internal fun DetailTrackHeader(fields: List<AndroidSortField>, displaySize: String?) {
    val imageSize = when (displaySize) { "Compact" -> 36.dp; "Large" -> 64.dp; else -> 48.dp }
    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically) {
        fields.forEach { field ->
            if (field.id == "Image" || field.id == "Tools") Spacer(detailColumn(field.id, imageSize))
            else if (field.id == "Duration") Box(detailColumn(field.id)) {
                RufinIcon("rufin-preferences-system-time-symbolic", translate("Duration"), Modifier.size(14.dp))
            } else Text(if (field.id == "TitleMerged") translate("Title") else field.title, detailColumn(field.id), maxLines = 1, overflow = TextOverflow.Ellipsis,
                textAlign = if (field.id == "RowIndex") TextAlign.Center else TextAlign.Start,
                style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
    HorizontalDivider(Modifier.padding(horizontal = 8.dp))
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DetailAlbumRow(item: BrowseItem, player: PlayerConnection, activate: (BrowseItem) -> Unit,
    menu: (BrowseItem) -> Unit, favorite: (BrowseItem) -> Unit,
) {
    val compact = item.displaySettings?.size == "Compact"
    val imageSize = when (item.displaySettings?.size) { "Compact" -> 36.dp; "Large" -> 64.dp; else -> 48.dp }
    val padding = if (compact) 0.dp else 6.dp
    BrowseSwipeActions(item, true) {
    Row(Modifier.fillMaxWidth().heightIn(min = 64.dp).combinedClickable(onClick = { activate(item) }, onLongClick = { menu(item) }).padding(start = 16.dp, end = 8.dp, top = padding, bottom = padding), verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        DetailCover(item.row, detailArtwork(item.row, player, imageSize), Modifier.size(imageSize))
        Column(Modifier.weight(1f)) {
            BrowseTitle(item.row, style = MaterialTheme.typography.bodyMedium, player = player)
            Text(item.row.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
            item.row.year?.let { Text(it.toString(), style = MaterialTheme.typography.bodySmall) }
        }
        PlayerIconButton("rufin-view-more-symbolic", "More", { menu(item) })
    }
    }
}


@Composable
private fun DetailLoadState(state: LoadState, retry: () -> Unit) {
    if (state is LoadState.Loading) Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
    else if (state is LoadState.Error) Column(Modifier.fillMaxWidth().padding(8.dp)) {
        io.github.screwys.rufin.ui.ErrorNotice(state.error)
        TextButton(retry) { Text(translate("Retry")) }
    }
}

private fun detailDuration(millis: ULong): String {
    val seconds = millis / 1000UL
    return if (seconds >= 3600UL) "${seconds / 3600UL}:${((seconds / 60UL) % 60UL).toString().padStart(2, '0')}:${(seconds % 60UL).toString().padStart(2, '0')}"
    else "${seconds / 60UL}:${(seconds % 60UL).toString().padStart(2, '0')}"
}
