package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.ChoiceChip

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize

import io.github.screwys.rufin.shared.rufin_core.Route
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
import io.github.screwys.rufin.ui.CoverShape
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
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.unit.dp
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
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
    onLink: (AndroidDetailLink) -> Unit, onScrolled: (Boolean) -> Unit, onRadio: (BrowseItem) -> Unit,
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
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            if (page.loading) CircularProgressIndicator() else Text(translate("This isn't available"))
        }
        return
    }
    val route = page.location.route
    val headerItem = remember(summary, page.pageView, page.displaySettings) { BrowseItem(summary.row, 0UL, page.pageView, page.displaySettings) }
    var selecting by remember { mutableStateOf(false) }
    if (selecting) PlaylistSelectMode(player.model, player.model.browse, page, rows, player, headerItem) { selecting = false }
    val artist = route is Route.ArtistDetail || route is Route.AlbumArtistDetail
    val releasesVisible = route.showsReleases
    fun images(fields: List<AndroidSortField>) = fields.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val width = maxWidth
        val findHeight = with(LocalDensity.current) { findOpen.toDp() }
        LazyColumn(Modifier.fillMaxSize().nestedScroll(reveal), state = state, contentPadding = PaddingValues(
            top = if (finding) LocalOverlayBarHeight.current else 0.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
            if (!finding) item("detail-header") { CompositionLocalProvider(LocalFindHeight provides findHeight) {
                DetailHeader(route, summary, favoriteItem(player.model, headerItem), player, onPlay, onFavorite, onOpenRoute, onLink, onMenu,
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
                    // A saved playlist's rows open its Select mode from above them.
                    if (headerItem.row.playlist != null && !finding) item("detail-select") {
                        Row(Modifier.fillMaxWidth().height(40.dp).padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                            Text(trackCountText(headerItem.row.trackCount), Modifier.weight(1f), style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant)
                            TextButton({ selecting = true }) {
                                RufinIcon("rufin-object-select-symbolic", null, Modifier.size(16.dp))
                                Spacer(Modifier.width(6.dp))
                                Text(translate("Select"))
                            }
                        }
                    }
                    if (display?.showHeader != false) item("detail-track-header") { FieldHeader(page.fields, display?.size) }
                    items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
                        val item = rows[index]
                        if (item == null) Spacer(Modifier.height(48.dp))
                        else FieldRow(item.copy(displaySettings = display), player,
                            images(page.fields), page.fields, onActivate, onMenu, onFavorite, onAddToPlaylist, allowSwipe = true)
                    }
                }
                item { DetailLoadState(rows.loadState.refresh, rows::retry); DetailLoadState(rows.loadState.append, rows::retry) }
                if (rows.itemCount == 0 && rows.loadState.refresh is LoadState.NotLoading) item {
                    BrowseEmptyState(page.hasActiveFilters, resetFilters = page::resetFilters)
                }
            }
        }
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

/** A detail page's header: Rufin's band behind the bar with the cover's top edge level with the
 *  bar's centre; on phones the cover takes its own row and the rest sits left-aligned under it,
 *  in wider panes it sits beside the text. Then the title, an album's artist, the facts ending
 *  with the links, the genres and the buttons. An artist's sub-pages show a plain band. */
@OptIn(ExperimentalLayoutApi::class, ExperimentalFoundationApi::class)
@Composable
private fun DetailHeader(
    route: Route, summary: AndroidDetailSummary, item: BrowseItem, player: PlayerConnection,
    onPlay: (BrowseItem, QueuePlacement, Boolean) -> Unit, onFavorite: (BrowseItem) -> Unit,
    onOpenRoute: (Route, String) -> Unit, onLink: (AndroidDetailLink) -> Unit, onMenu: (BrowseItem) -> Unit,
    onRadio: (BrowseItem) -> Unit, onDownload: (BrowseItem) -> Unit,
) {
    val bar = LocalOverlayBarHeight.current
    val albumCount = (item.row.artistRow?.summary?.albumCount ?: 0L).coerceAtLeast(0).toULong()
    val playlist = item.row.playlist != null || item.row.smartPlaylist != null
    val title = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.ExtraBold)
    val sectionTitle = when (route) {
        is Route.ArtistDiscography, is Route.AlbumArtistDiscography -> "Discography"
        is Route.ArtistFavoriteTracks, is Route.AlbumArtistFavoriteTracks -> "Favorite tracks"
        is Route.ArtistTracks, is Route.AlbumArtistTracks -> "Tracks"
        else -> null
    }
    if (sectionTitle != null) {
        Box(Modifier.fillMaxWidth()) {
            Box(Modifier.matchParentSize().then(showcaseBand(detailArtwork(item.row.artworkIdentity, player, 128.dp)?.band)))
            Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = bar + 4.dp, bottom = 18.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp)) {
                FindRow(summary.row.title, LocalFindHeight.current)
                BrowseTitle(item, maxLines = 1, style = title, marquee = true, showDownloadBadge = false)
                Text(translate(sectionTitle), style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.SemiBold)
                FactsPill(listOf(Triple("rufin-albums-symbolic", "Albums", albumCountText(albumCount)),
                    Triple("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount))), Modifier.padding(top = 6.dp))
            }
        }
        return
    }
    // Under a cover on its own row (small screens) the facts, links, genres and buttons are centred like the title.
    val facts: @Composable (Boolean) -> Unit = { centred ->
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = if (centred) Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally) else Arrangement.Start) {
            Box(if (centred) Modifier.weight(1f, fill = false) else Modifier.weight(1f)) {
                // An artist's counts are chips that open the albums and tracks; other pages' facts sit in the
                // showcase's pill.
                if (route is Route.ArtistDetail || route is Route.AlbumArtistDetail) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceChip(false, { onOpenRoute(if (route is Route.AlbumArtistDetail) Route.AlbumArtistDiscography(route.v1)
                            else Route.ArtistDiscography((route as Route.ArtistDetail).v1), translate("Albums")) },
                        label = { Text(albumCountText(albumCount)) },
                        leadingIcon = { RufinIcon("rufin-albums-symbolic", null, Modifier.size(16.dp)) })
                    ChoiceChip(false, { onOpenRoute(if (route is Route.AlbumArtistDetail) Route.AlbumArtistTracks(route.v1)
                            else Route.ArtistTracks((route as Route.ArtistDetail).v1), translate("Tracks")) },
                        label = { Text(trackCountText(item.row.trackCount)) },
                        leadingIcon = { RufinIcon("rufin-tracks-symbolic", null, Modifier.size(16.dp)) })
                } else FactsPill(listOfNotNull(item.row.year?.let { Triple("rufin-x-office-calendar-symbolic", "Year", it.toString()) },
                    Triple("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount)),
                    Triple("rufin-preferences-system-time-symbolic", "Duration", item.row.durationText)
                        .takeIf { item.row.durationMillis > 0UL || playlist }), onBand = false)
            }
            // The external links end the facts row; a playlist's is its source.
            CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                Row(horizontalArrangement = Arrangement.spacedBy(2.dp), verticalAlignment = Alignment.CenterVertically) {
                    if (playlist) player.model.sources?.sources?.find { it.id == item.row.sourceId }?.let { source ->
                        val link = summary.links.firstOrNull()
                        IconButton({ link?.let(onLink) }, Modifier.size(32.dp), enabled = link?.url != null) {
                            SourceIcon(source.kind, Modifier.size(20.dp).semantics { contentDescription = source.name })
                        }
                    } else summary.links.forEach { link ->
                        IconButton({ onLink(link) }, Modifier.size(32.dp).semantics { contentDescription = translate(link.title) }) {
                            DetailLinkIcon(link.iconId, Modifier.size(20.dp))
                        }
                    }
                }
            }
        }
    }
    val heading: @Composable () -> Unit = {
        BrowseTitle(item, maxLines = 1, style = title, marquee = true, showDownloadBadge = false)
        if (item.row.albumRow != null && summary.artistText.isNotBlank()) {
            val linkColor = MaterialTheme.colorScheme.onSurface
            val credited = buildAnnotatedString {
                append(summary.artistText)
                summary.artistLinks.forEach { artist ->
                    addLink(LinkAnnotation.Clickable(artist.route.toString(),
                        TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.None)),
                        { onOpenRoute(artist.route, artist.title) }), artist.start.toInt(), artist.end.toInt())
                }
            }
            Text(credited, Modifier.rufinMarquee().padding(top = 2.dp), maxLines = 1, style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold, color = linkColor)
        }
    }
    val buttons: @Composable (Boolean) -> Unit = { centred ->
        FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, if (centred) Alignment.CenterHorizontally else Alignment.Start),
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            ShowcasePill("rufin-media-playback-start-symbolic", "Play", true,
                { onPlay(item, QueuePlacement.Replace(0UL), false) }, { onPlay(item, QueuePlacement.Replace(0UL), true) })
            if (item.row.albumRow != null || item.row.artistRow != null || item.row.genre != null || item.row.playlist != null)
                ShowcasePill("rufin-audio-only-symbolic", "Radio", false, { onRadio(item) })
            if (item.row.albumRow != null || item.row.artistRow != null)
                ShowcaseRound(if (item.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", "Favorite",
                    { onFavorite(item) }, if (item.row.favorite) MaterialTheme.colorScheme.error else LocalContentColor.current)
            ShowcaseRound("rufin-download-symbolic", if (item.row.downloaded) "Remove Downloads" else "Download",
                { onDownload(item) }, if (item.row.downloaded) MaterialTheme.colorScheme.primary else LocalContentColor.current)
        }
    }
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val wide = maxWidth >= 560.dp
        val coverTop = (bar - 28.dp).coerceAtLeast(16.dp)
        val cover = if (wide) ((maxWidth - 32.dp) * .3f).coerceIn(164.dp, 224.dp) else (maxWidth - 32.dp).coerceAtMost(216.dp)
        val band = if (wide) bar + 128.dp else coverTop + cover * .54f
        val coverBox: @Composable () -> Unit = {
            Box(Modifier.size(cover).shadow(14.dp, CoverShape, ambientColor = Color.Black.copy(alpha = .4f),
                spotColor = Color.Black.copy(alpha = .4f)).pointerInput(item.row.key) { detectTapGestures(onLongPress = { onMenu(item) }) }) {
                DetailHeaderCover(summary, player, cover)
            }
        }
        // The band takes the colours of the cover the header shows (its first quadrant for a mosaic).
        val single = summary.artworkBindings.size <= 1
        val stops = detailArtwork(summary.artworkBindings.firstOrNull() ?: summary.row.artworkIdentity, player,
            if (single) cover else cover / 2f)?.band
        val bandSlot = LocalDetailBand.current
        SideEffect { bandSlot?.value = stops?.get(1) }
        // Above the cover, on the band, the Find field's row, as far as a pull has opened it.
        val find = LocalFindHeight.current
        Box(Modifier.fillMaxWidth().height(band + find).then(showcaseBand(stops)))
        FindRow(summary.row.title, find, Modifier.padding(start = 16.dp, end = 16.dp, top = bar))
        if (wide) Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = coverTop + find, bottom = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(24.dp)) {
            coverBox()
            Column(Modifier.weight(1f)) {
                Column(Modifier.fillMaxWidth().height(band - coverTop - 12.dp), verticalArrangement = Arrangement.Bottom) {
                    CompositionLocalProvider(LocalContentColor provides OnBand) { heading() }
                }
                Column(Modifier.padding(top = 24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    facts(false)
                    if (summary.genres.isNotEmpty()) GenreChips(summary.genres, false, onOpenRoute)
                }
                Spacer(Modifier.height(18.dp))
                buttons(false)
            }
        } else Column(Modifier.fillMaxWidth().padding(top = coverTop + find, bottom = 8.dp)) {
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { coverBox() }
            Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 22.dp)) {
                // Under the cover, the title and its line are centred on it.
                Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) { heading() }
                Spacer(Modifier.height(10.dp))
                facts(true)
                if (summary.genres.isNotEmpty()) {
                    Spacer(Modifier.height(14.dp))
                    GenreChips(summary.genres, true, onOpenRoute)
                }
                Spacer(Modifier.height(20.dp))
                buttons(true)
            }
        }
    }
}

/** The genres as chips filling one line; those that do not fit open from the last chip's +n. */
@Composable
private fun GenreChips(genres: List<io.github.screwys.rufin.core.AndroidGenreLink>, centred: Boolean, onOpen: (Route, String) -> Unit) {
    var overflow by remember { mutableStateOf(false) }
    androidx.compose.ui.layout.SubcomposeLayout(Modifier.fillMaxWidth()) { constraints ->
        val gap = 6.dp.roundToPx()
        val free = androidx.compose.ui.unit.Constraints()
        val chips = genres.mapIndexed { index, genre ->
            subcompose("genre:$index") { GenreChip(genre.title) { onOpen(genre.route, genre.title) } }.first().measure(free)
        }
        var shown = chips.size
        var more: androidx.compose.ui.layout.Placeable? = null
        if (chips.sumOf { it.width } + gap * (chips.size - 1).coerceAtLeast(0) > constraints.maxWidth) {
            val reserve = subcompose("probe") { GenreChip("+${chips.size}") {} }.first().measure(free).width + gap
            var used = 0
            shown = chips.takeWhile { chip -> (used + chip.width + gap + reserve <= constraints.maxWidth + gap).also { if (it) used += chip.width + gap } }.size
            val rest = genres.drop(shown)
            more = subcompose("more") {
                Box {
                    GenreChip("+${rest.size}") { overflow = true }
                    DropdownMenu(overflow, { overflow = false }) {
                        rest.forEach { genre -> DropdownMenuItem(text = { Text(genre.title) }, onClick = { overflow = false; onOpen(genre.route, genre.title) }) }
                    }
                }
            }.first().measure(free)
        }
        val placed = chips.take(shown)
        val used = (placed + listOfNotNull(more)).let { all -> all.sumOf { it.width } + gap * (all.size - 1).coerceAtLeast(0) }
        layout(constraints.maxWidth, (placed + listOfNotNull(more)).maxOfOrNull { it.height } ?: 0) {
            var x = if (centred) ((constraints.maxWidth - used) / 2).coerceAtLeast(0) else 0
            placed.forEach { it.placeRelative(x, 0); x += it.width + gap }
            more?.placeRelative(x, 0)
        }
    }
}

@Composable
private fun GenreChip(label: String, onClick: () -> Unit) {
    Box(Modifier.height(28.dp).clip(RoundedCornerShape(14.dp))
        .background(MaterialTheme.colorScheme.surfaceContainerHighest)
        .clickable(onClick = onClick).padding(horizontal = 12.dp), contentAlignment = Alignment.Center) {
        Text(label, maxLines = 1, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurface.copy(alpha = .88f))
    }
}

@Composable
private fun DetailHeaderCover(summary: AndroidDetailSummary, player: PlayerConnection, size: Dp) {
    val bindings = summary.artworkBindings
    if (bindings.size <= 1) {
        DetailCover(detailArtwork(bindings.firstOrNull() ?: summary.row.artworkIdentity, player, size), Modifier.size(size))
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

@Composable
private fun DetailSectionHeading(title: String) {
    Text(title, Modifier.fillMaxWidth().padding(start = 16.dp, top = 16.dp, bottom = 8.dp),
        style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
}

@Composable
private fun detailArtwork(identity: ByteArray?, player: PlayerConnection, size: Dp): PlayerArtwork? {
    val pixels = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
    return rememberArtwork(identity, player, pixels)
}

@Composable
private fun DetailCover(artwork: PlayerArtwork?, modifier: Modifier,
) {
    Box(modifier.clip(CoverShape)) {
        if (artwork == null) FallbackCover(Modifier.fillMaxSize()) else Artwork(artwork, Modifier.fillMaxSize())
    }
}

/** A list row showing the fields its list shows, as columns. */
@OptIn(ExperimentalFoundationApi::class)
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
    val member = item.row.playlistFolderMember
    Row(Modifier.fillMaxWidth().heightIn(min = rowHeight).playlistFolderDrop(item, player.model)
        .combinedClickable(onClick = { activate(item) }, onLongClick = { menu(projected) })
        .padding(start = 8.dp, end = 4.dp, top = rowPadding, bottom = rowPadding),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        fields.forEach { field ->
            val value = projected.row.fields.firstOrNull { it.id == field.id }?.text.orEmpty()
            when (field.id) {
                LibraryField.IMAGE -> if (showImage) DetailCover(detailArtwork(item.row.artworkIdentity, player, imageSize), Modifier.size(imageSize))
                LibraryField.TOOLS -> if (member != null) PlaylistDragHandle(member, detailColumn(field.id))
                    else PlayerIconButton("rufin-view-more-symbolic", "More", { menu(projected) }, detailColumn(field.id))
                LibraryField.FAVORITE -> IconButton({ favorite(projected) }, detailColumn(field.id)) {
                    RufinIcon(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", translate("Favorite"), Modifier.size(24.dp),
                        if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
                }
                LibraryField.ROW_INDEX -> Text(value, detailColumn(field.id), textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodySmall)
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
                else Text(value, detailColumn(field.id), maxLines = 2, overflow = TextOverflow.Ellipsis,
                    style = if (field.id == LibraryField.TITLE) MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodySmall)
            }
        }
        if (member != null && fields.none { it.id == LibraryField.TOOLS }) PlaylistDragHandle(member)
    }
    }
}

private fun RowScope.detailColumn(id: LibraryField, imageSize: Dp = 48.dp): Modifier = when (id) {
    LibraryField.ROW_INDEX -> Modifier.width(28.dp)
    LibraryField.DURATION -> Modifier.width(44.dp)
    LibraryField.YEAR -> Modifier.width(40.dp)
    LibraryField.IMAGE -> Modifier.width(imageSize)
    LibraryField.TOOLS, LibraryField.FAVORITE -> Modifier.width(48.dp)
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
