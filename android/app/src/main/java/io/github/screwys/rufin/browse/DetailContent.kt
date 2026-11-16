package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.PageLoading

import io.github.screwys.rufin.ui.ChoiceChip

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.Cover
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.DetailLinkIcon
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.player.LocalToolIconSize
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.player.PlayerConnection

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.layout.SubcomposeLayout
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
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import io.github.screwys.rufin.ui.focusRing
import io.github.screwys.rufin.ui.ListScrollbar
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
    summary: AndroidDetailSummary?, page: BrowsePage, rows: LazyPagingItems<BrowseItem>, player: PlayerConnection,
    releaseGroups: List<AndroidArtistReleaseGroup>,
    onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    onPlay: (BrowseItem, QueuePlacement, Boolean) -> Unit, onFavorite: (BrowseItem) -> Unit,
    onAddToPlaylist: (BrowseItem) -> Unit, onOpenRoute: (Route, String) -> Unit,
    onScrolled: (Boolean) -> Unit, onRadio: (BrowseItem) -> Unit,
    onDownload: (BrowseItem) -> Unit,
) {
    val favorites = page.favoritePages.collectAsLazyPagingItems()
    val state = rememberLazyListState()
    // A pull past the top opens the Find field's row above the cover, as Spotify's does; scrolling
    // up closes it before the page moves, and a release settles it open or closed.
    val findRow = with(LocalDensity.current) { if (LocalOpenFind.current != null) FIND_ROW.toPx() else 0f }
    var findOpen by remember { mutableFloatStateOf(0f) }
    val reveal = remember(findRow) { object : NestedScrollConnection {
        override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
            if (available.y >= 0f || findOpen <= 0f) return Offset.Zero
            val taken = maxOf(available.y, -findOpen)
            findOpen += taken
            return Offset(0f, taken)
        }
        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            if (available.y <= 0f || source != NestedScrollSource.UserInput || findOpen >= findRow) return Offset.Zero
            val taken = minOf(available.y, findRow - findOpen)
            findOpen += taken
            return Offset(0f, taken)
        }
    } }
    LaunchedEffect(state, findRow) {
        snapshotFlow { state.isScrollInProgress }.collect { scrolling ->
            if (!scrolling && findOpen > 0f && findOpen < findRow)
                androidx.compose.animation.core.animate(findOpen, if (findOpen > findRow / 2) findRow else 0f) { value, _ -> findOpen = value }
        }
    }
    // Find is its own view: the rows alone under the search bar, then back to the page's top.
    val finding = LocalFinding.current
    var found by remember { mutableStateOf(false) }
    LaunchedEffect(finding) {
        if (finding) found = true else if (found) { found = false; findOpen = 0f; state.scrollToItem(0) }
    }
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
            .distinctUntilChanged().collect(onScrolled)
    }
    if (summary == null) {
        if (page.loading) PageLoading()
        else Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { Text(translate("This isn't available")) }
        return
    }
    val route = page.location.route
    val headerItem = remember(summary, page.pageView, page.displaySettings) { BrowseItem(summary.row, 0UL, page.pageView, page.displaySettings) }
    val select = LocalSelectMode.current
    if (select.route == route) PlaylistSelectMode(player.model, player.model.browse, page, rows, player, headerItem) { select.route = null }
    val artist = route is Route.ArtistDetail || route is Route.AlbumArtistDetail
    val releasesVisible = route.showsReleases
    fun images(fields: List<AndroidSortField>) = fields.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val width = maxWidth
        val findHeight = with(LocalDensity.current) { findOpen.toDp() }
        LazyColumn(Modifier.fillMaxSize().nestedScroll(reveal), state = state, contentPadding = PaddingValues(
            top = if (finding) LocalOverlayBarHeight.current else 0.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
            if (!finding) item("detail-header") { CompositionLocalProvider(LocalFindHeight provides findHeight) {
                DetailHeader(route, summary, favoriteItem(player.model, headerItem), player, onPlay, onFavorite, onOpenRoute, onMenu,
                    onRadio, onDownload)
            } }
            if (artist && page.favoriteCount > 0UL) item("detail-favorites") {
                Column {
                    val display = favorites.itemSnapshotList.items.firstOrNull()?.displaySettings
                    DetailSectionHeading(translate("Favorite tracks"))
                    if (display?.showHeader != false) FieldHeader(page.favoriteFields, display?.size)
                    val visibleRows = if (favorites.loadState.refresh is LoadState.Loading)
                        minOf(page.favoriteCount, 4UL).toInt() else minOf(favorites.itemCount, 4)
                    val baseHeight = when (display?.size) { DisplaySize.COMPACT -> 48.dp; DisplaySize.LARGE -> 80.dp; else -> 64.dp }
                    val rowHeight = baseHeight * LocalDensity.current.fontScale.coerceAtLeast(1f)
                    if (favorites.itemCount == 0 && favorites.loadState.refresh is LoadState.NotLoading) {
                        BrowseEmptyState(page.options.filter.isNotBlank(), resetFilters = page::resetFilters)
                    } else LazyColumn(Modifier.fillMaxWidth().height(rowHeight * visibleRows.coerceAtLeast(1))) {
                        items(favorites.itemCount, key = favorites.itemKey { "${it.row.key}:${it.index}" }) { index ->
                            val item = favorites[index]
                            if (item == null) Spacer(Modifier.height(64.dp)) else FieldRow(item, player,
                                images(page.favoriteFields), page.favoriteFields, onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                        }
                        item { DetailLoadState(favorites.loadState.append, favorites::retry) }
                    }
                    DetailLoadState(favorites.loadState.refresh, favorites::retry)
                }
            }
            if (releasesVisible) {
                releaseGroups.filter { it.total > 0UL }.forEach { group ->
                    val display = page.options.displaySettings ?: page.displaySettings
                    val grid = display?.layout == LibraryLayout.GRID
                    val cardSize = when (display?.size) { DisplaySize.COMPACT -> 104.dp; DisplaySize.LARGE -> 176.dp; else -> 128.dp }
                    val gap = when (display?.gridSpacing) { DisplaySize.COMPACT -> 4.dp; DisplaySize.LARGE -> 16.dp; else -> 12.dp }
                    val columns = if (grid) ((width - 32.dp + gap) / (cardSize + gap)).toInt().coerceAtLeast(1) else 1
                    val count = group.total.toInt()
                    item("release-heading:${group.id}") { DetailSectionHeading(translate(group.title)) }
                    items((count + columns - 1) / columns, key = { "release:${group.id}:$it" }) { rowIndex ->
                            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(gap)) {
                                repeat(columns) { column ->
                                    val position = rowIndex * columns + column
                                    if (position >= count) Spacer(Modifier.weight(1f)) else {
                                        val index = group.index.toInt() + position
                                        val item = if (index < rows.itemCount) rows[index] else null
                                        Box(Modifier.weight(1f)) {
                                            if (item == null) Spacer(Modifier.height(if (grid) cardSize + 64.dp else 76.dp))
                                            else if (grid) CoverCard(item.copy(displaySettings = display), player, Modifier.fillMaxWidth().padding(bottom = 12.dp), onActivate, onMenu)
                                            else FieldRow(item.copy(displaySettings = display), player,
                                                page.fields.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }, page.fields,
                                                onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                                        }
                                    }
                                }
                            }
                        }
                }
                item("release-state") {
                    DetailLoadState(rows.loadState.refresh, rows::retry)
                    DetailLoadState(rows.loadState.append, rows::retry)
                }
            } else {
                val display = page.options.displaySettings ?: page.displaySettings
                if (display?.layout == LibraryLayout.GRID) {
                    val cardSize = when (display.size) { DisplaySize.COMPACT -> 104.dp; DisplaySize.LARGE -> 176.dp; else -> 128.dp }
                    val gap = when (display.gridSpacing) { DisplaySize.COMPACT -> 4.dp; DisplaySize.LARGE -> 16.dp; else -> 12.dp }
                    val columns = ((width - 32.dp + gap) / (cardSize + gap)).toInt().coerceAtLeast(1)
                    items((rows.itemCount + columns - 1) / columns, key = { "detail-grid:$it" }) { rowIndex ->
                        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(gap)) {
                            repeat(columns) { column ->
                                val position = rowIndex * columns + column
                                if (position >= rows.itemCount) Spacer(Modifier.weight(1f)) else Box(Modifier.weight(1f)) {
                                    val item = rows[position]
                                    if (item == null) Spacer(Modifier.height(cardSize + 48.dp))
                                    else CoverCard(item.copy(displaySettings = display), player, Modifier.fillMaxWidth(), onActivate, onMenu)
                                }
                            }
                        }
                    }
                } else {
                    if (display?.showHeader != false) item("detail-track-header") { FieldHeader(page.fields, display?.size) }
                    items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
                        val item = rows[index]
                        Column {
                            page.discHeadings[index]?.let { DiscHeading(it) }
                            if (item == null) Spacer(Modifier.height(48.dp))
                            else FieldRow(item.copy(displaySettings = display), player,
                                images(page.fields), page.fields, onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                        }
                    }
                }
                item { DetailLoadState(rows.loadState.refresh, rows::retry); DetailLoadState(rows.loadState.append, rows::retry) }
                if (rows.itemCount == 0 && rows.loadState.refresh is LoadState.NotLoading) item {
                    BrowseEmptyState(page.hasActiveFilters, resetFilters = page::resetFilters)
                }
            }
        }
        // Every page shows where it is, beside a list or alone.
        ListScrollbar(state, Modifier.align(Alignment.CenterEnd).padding(top = LocalOverlayBarHeight.current, bottom = LocalPlayerBottomPadding.current))
    }
}

/** Whether the detail page shows Find's results: its rows without the header. */
internal val LocalFinding = compositionLocalOf { false }

/** Opens Find from the field at the top of a detail page's header; null where the page has none. */
internal val LocalOpenFind = compositionLocalOf<(() -> Unit)?> { null }

/** The room the Find field takes above the cover, open. */
private val FIND_ROW = 76.dp

/** How far a pull has opened the Find row. */
private val LocalFindHeight = compositionLocalOf { 0.dp }

/** The Find field's row, `height` of its full height open, the field sliding down from behind the bar. */
@Composable
private fun FindRow(name: String, height: Dp, modifier: Modifier = Modifier) {
    val open = LocalOpenFind.current ?: return
    Box(modifier.fillMaxWidth().height(height).clipToBounds()) {
        FindField(name, open, Modifier.align(Alignment.BottomStart).padding(bottom = FIND_ROW - 44.dp))
    }
}

/** "Find in …" on the band; a tap opens Find's rows. */
@Composable
private fun FindField(name: String, open: () -> Unit, modifier: Modifier) {
    Row(modifier.fillMaxWidth().height(40.dp).clip(RoundedCornerShape(8.dp)).background(OnBand.copy(alpha = .16f))
        .clickable(onClick = open).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        RufinIcon("rufin-search-symbolic", null, Modifier.size(18.dp), OnBand)
        Text(translate("Find in {name}").replace("{name}", name), maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.bodyMedium, color = OnBand)
    }
}

/** The name of the section an artist's sub-page shows, which has a plain band for a header. */
private val Route.sectionTitle get() = when (this) {
    is Route.ArtistDiscography, is Route.AlbumArtistDiscography -> "Discography"
    is Route.ArtistFavoriteTracks, is Route.AlbumArtistFavoriteTracks -> "Favorite tracks"
    is Route.ArtistTracks, is Route.AlbumArtistTracks -> "Tracks"
    else -> null
}

/** A detail page's header, starting under the bar, which carries the page's links (`DetailLinks`). On a
 *  fold or tablet Rufin's band holds the cover over its lower edge and beside it the title, an album's
 *  artist and one line of the facts and genres, with only the buttons under the band, beside the cover.
 *  On a phone the band holds a small cover and beside it the title, whole, and the artist; the facts and
 *  genres, then the buttons, take the page's width under it. An artist's sub-pages show a plain band. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DetailHeader(
    route: Route, summary: AndroidDetailSummary, item: BrowseItem, player: PlayerConnection,
    onPlay: (BrowseItem, QueuePlacement, Boolean) -> Unit, onFavorite: (BrowseItem) -> Unit,
    onOpenRoute: (Route, String) -> Unit, onMenu: (BrowseItem) -> Unit,
    onRadio: (BrowseItem) -> Unit, onDownload: (BrowseItem) -> Unit,
) {
    val bar = LocalOverlayBarHeight.current
    val albumCount = (item.row.artistRow?.summary?.albumCount ?: 0L).coerceAtLeast(0).toULong()
    val playlist = item.row.playlist != null || item.row.smartPlaylist != null
    val title = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.ExtraBold)
    val sectionTitle = route.sectionTitle
    if (sectionTitle != null) {
        Box(Modifier.fillMaxWidth()) {
            Box(Modifier.matchParentSize().then(showcaseBand(detailArtwork(item.row.artworkIdentity, player, 128.dp).image?.band)))
            Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = bar + 4.dp, bottom = 18.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp)) {
                FindRow(summary.row.title, LocalFindHeight.current)
                BrowseTitle(item, maxLines = 1, style = title, marquee = true, showDownloadBadge = false)
                Text(translate(sectionTitle), style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.SemiBold)
            }
        }
        return
    }
    val names: @Composable (Boolean) -> Unit = { phone ->
        CompositionLocalProvider(LocalContentColor provides OnBand) {
            // A phone's title is never cut: it wraps, a long one in a smaller size.
            if (phone) BrowseTitle(item, maxLines = Int.MAX_VALUE, showDownloadBadge = false,
                style = if (item.row.title.length > 40) title.copy(fontSize = 20.sp, lineHeight = 26.sp) else title)
            else BrowseTitle(item, maxLines = 1, style = title, marquee = true, showDownloadBadge = false)
            if (item.row.albumRow != null && summary.artistText.isNotBlank()) {
                val credited = buildAnnotatedString {
                    append(summary.artistText)
                    summary.artistLinks.forEach { artist ->
                        addLink(LinkAnnotation.Clickable(artist.route.toString(),
                            TextLinkStyles(SpanStyle(color = OnBand, textDecoration = TextDecoration.None)),
                            { onOpenRoute(artist.route, artist.title) }), artist.start.toInt(), artist.end.toInt())
                    }
                }
                Text(credited, Modifier.rufinMarquee().padding(top = 2.dp), maxLines = 1, style = MaterialTheme.typography.bodyLarge,
                    fontWeight = FontWeight.SemiBold, color = OnBand)
            }
        }
    }
    // The facts and the genres are one line. An artist's counts are chips that open the albums and tracks;
    // other pages' facts sit in the showcase's pill.
    val meta: @Composable (Boolean, Modifier) -> Unit = { onBand, place ->
        MetaLine(summary.genres.map { it.title }, onBand, place, { index -> summary.genres[index].let { onOpenRoute(it.route, it.title) } }) {
            // The counts slide, as a pill's facts do, when they are wider than their room.
            if (route is Route.ArtistDetail || route is Route.AlbumArtistDetail) CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
              Row(Modifier.rufinMarquee(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                ChoiceChip(false, { onOpenRoute(if (route is Route.AlbumArtistDetail) Route.AlbumArtistDiscography(route.v1)
                        else Route.ArtistDiscography((route as Route.ArtistDetail).v1), translate("Albums")) },
                    label = { Text(albumCountText(albumCount)) },
                    leadingIcon = { RufinIcon("rufin-albums-symbolic", null, Modifier.size(16.dp)) }, onBand = onBand)
                ChoiceChip(false, { onOpenRoute(if (route is Route.AlbumArtistDetail) Route.AlbumArtistTracks(route.v1)
                        else Route.ArtistTracks((route as Route.ArtistDetail).v1), translate("Tracks")) },
                    label = { Text(trackCountText(item.row.trackCount)) },
                    leadingIcon = { RufinIcon("rufin-tracks-symbolic", null, Modifier.size(16.dp)) }, onBand = onBand)
              }
            } else FactsPill(listOfNotNull(item.row.yearText?.let { Triple("rufin-x-office-calendar-symbolic", "Year", it) },
                Triple("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount)),
                Triple("rufin-preferences-system-time-symbolic", "Duration", item.row.durationText)
                    .takeIf { item.row.durationMillis > 0UL || playlist }), onBand = onBand)
        }
    }
    val buttons: @Composable (Int) -> Unit = { labels ->
        ShowcasePill("rufin-media-playback-start-symbolic", "Play", true,
            { onPlay(item, QueuePlacement.Replace(0UL), false) }, { onPlay(item, QueuePlacement.Replace(0UL), true) }, labels == 2)
        if (item.row.albumRow != null || item.row.artistRow != null || item.row.genre != null || item.row.playlist != null)
            ShowcasePill("rufin-audio-only-symbolic", "Radio", false, { onRadio(item) }, labelled = labels >= 1)
        if (item.row.albumRow != null || item.row.artistRow != null)
            ShowcaseRound(if (item.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", "Favorite",
                { onFavorite(item) }, if (item.row.favorite) MaterialTheme.colorScheme.error else LocalContentColor.current)
        ShowcaseRound("rufin-download-symbolic", if (item.row.downloaded) "Remove Downloads" else "Download",
            { onDownload(item) }, if (item.row.downloaded) MaterialTheme.colorScheme.primary else LocalContentColor.current)
    }
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val phone = maxWidth < 560.dp
        val counts = route is Route.ArtistDetail || route is Route.AlbumArtistDetail
        val cover = if (phone) 122.dp else ((maxWidth - 32.dp) * .3f).coerceIn(164.dp, 224.dp)
        // A tap shows the artwork whole, as a click on GTK's cover does; a long press opens the menu.
        var viewing by remember { mutableStateOf(false) }
        val coverBox: @Composable () -> Unit = {
            Box(Modifier.size(cover).shadow(14.dp, CoverShape, ambientColor = Color.Black.copy(alpha = .4f),
                spotColor = Color.Black.copy(alpha = .4f))
                .pointerInput(item.row.key) { detectTapGestures(onTap = { viewing = true }, onLongPress = { onMenu(item) }) }) {
                DetailHeaderCover(summary, player, cover)
            }
        }
        if (viewing) FullArtwork(summary, player) { viewing = false }
        // The band takes the colours of the cover the header shows (its first quadrant for a mosaic).
        val single = summary.artworkBindings.size <= 1
        val stops = detailArtwork(summary.artworkBindings.firstOrNull() ?: summary.row.artworkIdentity, player,
            if (single) cover else cover / 2f).image?.band
        val bandSlot = LocalDetailBand.current
        SideEffect { bandSlot?.value = stops?.get(1) }
        // Above the cover, on the band, the Find field's row, as far as a pull has opened it.
        val find = LocalFindHeight.current
        // The cover and the names start under the bar.
        val top = bar + find + 4.dp
        if (phone) Column(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
            // The band holds the cover and the names, centred on it or growing the band past it.
            Row(Modifier.fillMaxWidth().then(showcaseBand(stops)).padding(start = 16.dp, end = 16.dp, top = top, bottom = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                coverBox()
                Column(Modifier.weight(1f).heightIn(min = cover), verticalArrangement = Arrangement.Center) {
                    names(true)
                    // An artist's counts stay on the band, under the name.
                    if (counts) meta(true, Modifier.padding(top = 8.dp))
                }
            }
            Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (!counts) meta(false, Modifier)
                ButtonRow(buttons)
            }
        } else {
            val textStart = 16.dp + cover + 24.dp
            Column(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
                // The band grows with the names and the meta line from its least height; they end just above its edge.
                Column(Modifier.fillMaxWidth().then(showcaseBand(stops))
                    .padding(start = textStart, end = 16.dp, top = top, bottom = 12.dp).heightIn(min = 112.dp),
                    verticalArrangement = Arrangement.Bottom) {
                    names(false)
                    meta(true, Modifier.padding(top = 6.dp))
                }
                // The buttons start under the band's edge, beside the cover.
                Box(Modifier.padding(start = textStart, end = 16.dp, top = 24.dp)) { ButtonRow(buttons) }
            }
            Box(Modifier.padding(start = 16.dp, top = top, bottom = 8.dp)) { coverBox() }
        }
        FindRow(summary.row.title, find, Modifier.padding(start = 16.dp, end = 16.dp, top = bar))
    }
}

/** A detail page's external links, one dark group for the bar over its band; a playlist's is its source.
 *  Nothing where the page has none. */
@Composable
internal fun DetailLinks(route: Route, summary: AndroidDetailSummary, player: PlayerConnection, onLink: (AndroidDetailLink) -> Unit,
    modifier: Modifier = Modifier) {
    val playlist = summary.row.playlist != null || summary.row.smartPlaylist != null
    val source = if (playlist) player.model.sources?.sources?.find { it.id == summary.row.sourceId } else null
    if (route.sectionTitle != null || if (playlist) source == null else summary.links.isEmpty()) return
    // Each link is a bar button: the size and the spacing of the tools beside it.
    val icon = Modifier.size(LocalToolIconSize.current)
    CompositionLocalProvider(LocalContentColor provides OnBand) {
        Row(modifier.clip(RoundedCornerShape(50)).background(Color.Black.copy(alpha = .28f)),
            verticalAlignment = Alignment.CenterVertically) {
            if (source != null) {
                val link = summary.links.firstOrNull()
                IconButton({ link?.let(onLink) }, enabled = link?.url != null) {
                    SourceIcon(source.kind, icon.semantics { contentDescription = source.name })
                }
            } else summary.links.forEach { link ->
                IconButton({ onLink(link) }, Modifier.semantics { contentDescription = translate(link.title) }) {
                    DetailLinkIcon(link.iconId, icon)
                }
            }
        }
    }
}

/** A header's buttons, always one row: Play, then Radio, give up their labels while the row is wider than
 *  its room. `buttons` draws them with that many labels. */
@Composable
internal fun ButtonRow(buttons: @Composable (Int) -> Unit) {
    SubcomposeLayout(Modifier.fillMaxWidth()) { constraints ->
        var row = emptyList<Placeable>()
        for (labels in 2 downTo 0) {
            row = subcompose(labels) { buttons(labels) }.map { it.measure(Constraints()) }
            if (row.sumOf { it.width } + 8.dp.roundToPx() * (row.size - 1) <= constraints.maxWidth) break
        }
        // Round, they still close up before any leaves the row.
        val gap = minOf(8.dp.roundToPx(), (constraints.maxWidth - row.sumOf { it.width }) / (row.size - 1).coerceAtLeast(1)).coerceAtLeast(0)
        layout(constraints.maxWidth, row.maxOfOrNull { it.height } ?: 0) {
            var x = 0
            row.forEach { it.placeRelative(x, 0); x += it.width + gap }
        }
    }
}

/** A header's meta line: its facts, then its genres as chips, all on one line. The genres that do not fit
 *  wait behind a last chip, +n or › where none fits, which opens the line onto as many lines as they need;
 *  ‹ folds it again. A chip opens its genre by its place in the list, where the page has one to open. */
@Composable
internal fun MetaLine(genres: List<String>, onBand: Boolean, modifier: Modifier = Modifier, onOpen: ((Int) -> Unit)? = null,
    facts: @Composable () -> Unit) {
    var open by remember { mutableStateOf(false) }
    SubcomposeLayout(modifier.fillMaxWidth()) { constraints ->
        val gap = 6.dp.roundToPx()
        val width = constraints.maxWidth
        val chips = genres.mapIndexed { index, genre ->
            subcompose("genre:$index") { GenreChip(genre, onBand, onOpen?.let { opens -> { opens(index) } }) }.first()
                .measure(Constraints(maxWidth = width))
        }
        val span = chips.sumOf { it.width + gap }
        // The facts come first at their own width, short only of the room the last chip needs, as +n or as ›.
        val more = if (chips.isEmpty()) 0 else subcompose("probe") {
            GenreChip("+${chips.size}", onBand, null)
            GenreChip(translate("Genres"), onBand, null, "rufin-go-next-symbolic")
        }.maxOf { it.measure(Constraints()).width }
        var room = width - minOf(span, more + gap)
        val lead = subcompose("facts", facts).map { fact ->
            fact.measure(Constraints(maxWidth = room.coerceAtLeast(0))).also { room -= it.width + gap }
        }
        val start = lead.sumOf { it.width + gap }
        val folds = start + span - gap > width
        var shown = chips.size
        if (folds && !open) {
            var used = start
            shown = chips.takeWhile { chip -> (used + chip.width + gap + more <= width).also { fits -> if (fits) used += chip.width + gap } }.size
        }
        val toggle = if (folds) subcompose("toggle") {
            if (!open && shown > 0) GenreChip("+${chips.size - shown}", onBand, { open = !open })
            else GenreChip(translate("Genres"), onBand, { open = !open }, if (open) "rufin-go-previous-symbolic" else "rufin-go-next-symbolic")
        }.first().measure(Constraints()) else null
        val lines = mutableListOf(mutableListOf<Placeable>())
        var x = 0
        (lead + chips.take(shown) + listOfNotNull(toggle)).forEach { item ->
            if (open && x > 0 && x + item.width > width) { lines.add(mutableListOf()); x = 0 }
            lines.last().add(item)
            x += item.width + gap
        }
        // A line is a chip's height with or without chips, so a header keeps its height.
        val heights = lines.map { line -> maxOf(line.maxOfOrNull { it.height } ?: 0, 28.dp.roundToPx()) }
        layout(width, heights.sum() + gap * (lines.size - 1)) {
            var y = 0
            lines.forEachIndexed { index, line ->
                var at = 0
                line.forEach { it.placeRelative(at, y + (heights[index] - it.height) / 2); at += it.width + gap }
                y += heights[index] + gap
            }
        }
    }
}

/** A genre's chip. On the band one that opens something takes a control's light fill, one that only tells
 *  the facts' dark fill. With an `icon` it shows that in place of its label, which then names it. */
@Composable
private fun GenreChip(label: String, onBand: Boolean, onClick: (() -> Unit)?, icon: String? = null) {
    val fill = if (!onBand) MaterialTheme.colorScheme.surfaceContainerHighest
        else if (onClick != null) Color.White.copy(alpha = .17f) else Color.Black.copy(alpha = .28f)
    val text = if (onBand) OnBand else MaterialTheme.colorScheme.onSurface.copy(alpha = .88f)
    Box(Modifier.height(28.dp).clip(RoundedCornerShape(14.dp)).background(fill)
        .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier).padding(horizontal = 12.dp), contentAlignment = Alignment.Center) {
        if (icon != null) RufinIcon(icon, label, Modifier.size(16.dp), text)
        else Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelLarge, color = text)
    }
}

/** The header's artwork over the whole screen, as large as its shorter side allows; a tap or Back closes it. */
@Composable
private fun FullArtwork(summary: AndroidDetailSummary, player: PlayerConnection, onDismiss: () -> Unit) {
    androidx.compose.ui.window.Dialog(onDismiss, androidx.compose.ui.window.DialogProperties(usePlatformDefaultWidth = false)) {
        BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black.copy(alpha = .86f))
            .pointerInput(Unit) { detectTapGestures(onTap = { onDismiss() }) }, contentAlignment = Alignment.Center) {
            val side = minOf(maxWidth, maxHeight) - 32.dp
            Box(Modifier.size(side).clip(CoverShape)) { DetailHeaderCover(summary, player, side) }
        }
    }
}

@Composable
private fun DetailHeaderCover(summary: AndroidDetailSummary, player: PlayerConnection, size: Dp) {
    val bindings = summary.artworkBindings
    if (bindings.size <= 1) {
        DetailCover(detailArtwork(bindings.firstOrNull() ?: summary.row.artworkIdentity, player, size, animated = true), Modifier.size(size))
    } else {
        Column(Modifier.size(size).clip(CoverShape)) {
            repeat(2) { y ->
                Row {
                    repeat(2) { x ->
                        Artwork(detailArtwork(bindings[(y * 2 + x) % bindings.size], player, size / 2f), Modifier.size(size / 2f))
                    }
                }
            }
        }
    }
}

/** A disc's number and title over its first track, dimmed as GTK's disc headers are. */
@Composable
private fun DiscHeading(title: String) {
    Text(title, Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 4.dp),
        style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
}

@Composable
private fun DetailSectionHeading(title: String) {
    Text(title, Modifier.fillMaxWidth().padding(start = 16.dp, top = 16.dp, bottom = 8.dp),
        style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
}

@Composable
private fun detailArtwork(identity: ByteArray?, player: PlayerConnection, size: Dp, animated: Boolean = false): Cover {
    val pixels = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
    return rememberArtwork(identity, player, pixels, animated)
}

@Composable
private fun DetailCover(artwork: Cover, modifier: Modifier,
) {
    Box(modifier.clip(CoverShape)) {
        Artwork(artwork, Modifier.fillMaxSize())
    }
}

/** A list row showing the fields its list shows, as columns. */
@OptIn(ExperimentalFoundationApi::class)
/** The route of the page open beside a list, whose row shows as picked. */
internal val LocalSelectedRoute = compositionLocalOf<io.github.screwys.rufin.shared.rufin_core.Route?> { null }

/** A list row: picked while its page is open beside the list, ringed while a D-pad or keyboard is on it. */
@Composable
internal fun Modifier.pickedRow(item: BrowseItem): Modifier {
    val picked = item.row.route != null && item.row.route == LocalSelectedRoute.current
    return clip(RoundedCornerShape(8.dp))
        .background(if (picked) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent)
        .focusRing(RoundedCornerShape(8.dp))
}

@Composable
internal fun FieldRow(
    item: BrowseItem, player: PlayerConnection, showImage: Boolean, fields: List<AndroidSortField>,
    activate: (BrowseItem) -> Unit, menu: (BrowseItem) -> Unit,
    favorite: (BrowseItem) -> Unit, playlist: (BrowseItem) -> Unit, allowSwipe: Boolean = true,
) {
    val projected = favoriteItem(player.model, item)
    val size = item.displaySettings?.size
    val merged = fields.any { it.id == LibraryField.TITLE_MERGED }
    val imageSize = when (size) { DisplaySize.COMPACT -> 36.dp; DisplaySize.LARGE -> 64.dp; else -> 48.dp }
    val rowPadding = if (size == DisplaySize.COMPACT) 0.dp else 4.dp
    val baseHeight = when (size) { DisplaySize.COMPACT -> 48.dp; DisplaySize.LARGE -> 80.dp; else -> if (merged) 64.dp else 56.dp }
    val rowHeight = baseHeight * LocalDensity.current.fontScale.coerceAtLeast(1f)
    BrowseSwipeActions(item, allowSwipe) {
    Row(Modifier.fillMaxWidth().heightIn(min = rowHeight).pickedRow(item)
        .combinedClickable(onClick = { activate(item) }, onLongClick = { menu(projected) })
        .padding(start = 8.dp, end = 4.dp, top = rowPadding, bottom = rowPadding),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        fields.forEach { field ->
            val value = projected.row.fields.firstOrNull { it.id == field.id }?.text.orEmpty()
            when (field.id) {
                LibraryField.IMAGE -> if (showImage) DetailCover(detailArtwork(item.row.artworkIdentity, player, imageSize), Modifier.size(imageSize))
                // Compact actions, so the title keeps the width.
                LibraryField.TOOLS -> PlayerIconButton("rufin-view-more-symbolic", "More", { menu(projected) }, detailColumn(field.id).height(36.dp), iconSize = 20.dp)
                LibraryField.FAVORITE -> IconButton({ favorite(projected) }, detailColumn(field.id).height(36.dp)) {
                    RufinIcon(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", translate("Favorite"), Modifier.size(20.dp),
                        if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
                }
                // One line whatever the count: four digits fit, longer ones run into the gap beside them.
                LibraryField.ROW_INDEX -> Text(value, detailColumn(field.id), textAlign = TextAlign.Center, maxLines = 1, softWrap = false,
                    overflow = TextOverflow.Visible, style = MaterialTheme.typography.bodySmall)
                LibraryField.TITLE_MERGED -> Row(detailColumn(field.id), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (showImage) DetailCover(detailArtwork(item.row.artworkIdentity, player, imageSize), Modifier.size(imageSize))
                    Column(Modifier.weight(1f)) {
                        BrowseTitle(projected, maxLines = 1, style = MaterialTheme.typography.bodyMedium, player = player, title = value)
                        Text(item.row.artist, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        item.row.lyricsSnippet?.let { LyricsSnippetText(it) }
                    }
                }
                else -> if (field.id == LibraryField.TITLE) Column(detailColumn(field.id)) {
                    BrowseTitle(projected, maxLines = 2, style = MaterialTheme.typography.bodyMedium, player = player, title = value)
                    item.row.lyricsSnippet?.let { LyricsSnippetText(it) }
                }
                // A row found by its lyrics gives their line the room of the album and the like, as GTK's does.
                else if (item.row.lyricsSnippet != null && field.id !in setOf(LibraryField.DURATION, LibraryField.YEAR)) Unit
                else Text(value, detailColumn(field.id), maxLines = 2, overflow = TextOverflow.Ellipsis,
                    style = if (field.id == LibraryField.TITLE) MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodySmall)
            }
        }
    }
    }
}

private fun RowScope.detailColumn(id: LibraryField, imageSize: Dp = 48.dp): Modifier = when (id) {
    LibraryField.ROW_INDEX -> Modifier.width(34.dp)
    LibraryField.DURATION -> Modifier.width(44.dp)
    LibraryField.YEAR -> Modifier.width(40.dp)
    LibraryField.IMAGE -> Modifier.width(imageSize)
    LibraryField.TOOLS, LibraryField.FAVORITE -> Modifier.width(36.dp)
    LibraryField.TITLE, LibraryField.TITLE_MERGED -> Modifier.weight(1f)
    else -> Modifier.weight(.65f)
}

@Composable
internal fun FieldHeader(fields: List<AndroidSortField>, displaySize: DisplaySize?) {
    val imageSize = when (displaySize) { DisplaySize.COMPACT -> 36.dp; DisplaySize.LARGE -> 64.dp; else -> 48.dp }
    // The same columns and padding as FieldRow, so each heading sits over its values.
    Row(Modifier.fillMaxWidth().padding(start = 8.dp, end = 4.dp, top = 6.dp, bottom = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically) {
        fields.forEach { field ->
            if (field.id == LibraryField.IMAGE || field.id == LibraryField.TOOLS) Spacer(detailColumn(field.id, imageSize))
            else if (field.id == LibraryField.DURATION) Box(detailColumn(field.id), contentAlignment = Alignment.CenterStart) {
                RufinIcon("rufin-preferences-system-time-symbolic", translate("Duration"), Modifier.size(14.dp))
            } else if (field.id == LibraryField.FAVORITE) Box(detailColumn(field.id), contentAlignment = Alignment.Center) {
                RufinIcon("rufin-heart-outline-symbolic", translate("Favorite"), Modifier.size(14.dp))
            } else Text(if (field.id == LibraryField.TITLE_MERGED) translate("Title") else field.title, detailColumn(field.id), maxLines = 1, overflow = TextOverflow.Ellipsis,
                textAlign = if (field.id == LibraryField.ROW_INDEX) TextAlign.Center else TextAlign.Start,
                style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
    HorizontalDivider(Modifier.padding(horizontal = 8.dp))
}

@Composable
private fun DetailLoadState(state: LoadState, retry: () -> Unit) {
    if (state is LoadState.Loading) Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
    else if (state is LoadState.Error) Column(Modifier.fillMaxWidth().padding(8.dp)) {
        io.github.screwys.rufin.ui.ErrorNotice(state.error)
        TextButton(retry) { Text(translate("Retry")) }
    }
}
