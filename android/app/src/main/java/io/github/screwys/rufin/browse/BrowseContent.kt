package io.github.screwys.rufin.browse

import androidx.compose.ui.unit.toSize

import androidx.compose.ui.geometry.Size

import androidx.compose.ui.graphics.drawscope.DrawScope

import androidx.compose.ui.draw.drawBehind


import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.library.LyricsSnippet
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.Cover
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.FallbackCover
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.PlayingIndicator
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.player.PlayerConnection

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.shape.RoundedCornerShape
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.foundation.border
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Velocity
import androidx.paging.LoadState
import androidx.paging.compose.LazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.core.AndroidScrollSection
import io.github.screwys.rufin.core.AndroidSortField
import io.github.screwys.rufin.core.AndroidHomeSectionKind
import io.github.screwys.rufin.shared.library.Row as LibraryRow
import io.github.screwys.rufin.shared.rufin_core.playsContext
import kotlinx.coroutines.launch
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.key.key
import io.github.screwys.rufin.core.AndroidBrowseRow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.collect

@Composable
internal fun BrowseContent(
    items: LazyPagingItems<BrowseItem>, homeSections: List<HomeSectionItems>, player: PlayerConnection,
    home: Boolean, detail: Boolean, covers: Boolean, onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    onSectionPlay: (HomeSectionItems, Boolean) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onAddToPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit, scrollSections: List<AndroidScrollSection> = emptyList(),
    displaySize: DisplaySize = DisplaySize.DEFAULT, gridSpacing: DisplaySize = DisplaySize.DEFAULT, onPlayItem: (BrowseItem, Boolean) -> Unit = { item, _ -> onActivate(item) },
    fields: List<AndroidSortField> = emptyList(),
    letterIndex: Boolean = false,
    onLetterPosition: suspend (String) -> ULong? = { null },
    letterDescending: Boolean = false,
    filtered: Boolean = false, onResetFilters: () -> Unit = {},
    loading: Boolean = false,
    onRadio: (BrowseItem) -> Unit,
    emptyIcon: String? = null,
    onStepShowcase: suspend (Boolean) -> List<AndroidBrowseRow> = { emptyList() },
) {
    val coverWidth = when (displaySize) { DisplaySize.COMPACT -> 104.dp; DisplaySize.LARGE -> 176.dp; else -> 128.dp }
    val spacing = when (gridSpacing) { DisplaySize.COMPACT -> 4.dp; DisplaySize.LARGE -> 16.dp; else -> 12.dp }
    val tiny = io.github.screwys.rufin.app.LocalTinyScreen.current
    if (home) {
        HomeContent(homeSections, player, onActivate, onMenu, onSectionPlay, onFavorite, onScrolled, onRadio, onStepShowcase)
    } else {
        val scope = rememberCoroutineScope()
        val refresh = if (loading && items.itemCount == 0) LoadState.Loading else items.loadState.refresh
        val append = items.loadState.append
        val labels = remember(scrollSections, items.itemCount) {
            scrollSections.filter { it.title.isNotBlank() && it.index < items.itemCount.toULong() }
        }
        val alphabet = remember { listOf("#") + ('A'..'Z').map(Char::toString) }
        val letters = remember(alphabet, labels, letterDescending) {
            val extra = labels.map { it.title }.filter { it !in alphabet }.distinct()
            if (letterDescending) extra + alphabet.asReversed() else alphabet + extra
        }
        val indexed = items.itemCount > 0 && (letterIndex || labels.isNotEmpty())
        val density = LocalDensity.current
        val railStyle = MaterialTheme.typography.labelSmall
        val railMeasurer = rememberTextMeasurer()
        val railWidth = remember(letters, railStyle, railMeasurer, density.density, density.fontScale) {
            with(density) {
                letters.maxOf { railMeasurer.measure(AnnotatedString(it), railStyle, maxLines = 1).size.width }.toDp() + 4.dp
            }.coerceAtLeast(22.dp)
        }
        val indexInset = if (indexed) railWidth else 0.dp
        if (covers) {
            val state = rememberLazyGridState()
            LaunchedEffect(state) {
                snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
                    .distinctUntilChanged().collect(onScrolled)
            }
            Box(Modifier.fillMaxSize()) {
            LazyVerticalGrid(if (tiny) GridCells.Fixed(1) else GridCells.Adaptive(coverWidth), Modifier.fillMaxSize().padding(end = indexInset), state = state,
                contentPadding = PaddingValues(start = 16.dp, top = 12.dp, end = 16.dp, bottom = 12.dp + LocalPlayerBottomPadding.current), horizontalArrangement = Arrangement.spacedBy(spacing),
                verticalArrangement = Arrangement.spacedBy(spacing + 4.dp)) {
                items(items.itemCount, key = items.itemKey(::browseItemKey)) { index ->
                    val item = items[index]
                    if (item == null) CoverPlaceholder()
                    else CoverCard(item, player, Modifier.fillMaxWidth(), onActivate, onMenu)
                }
                if (append is LoadState.Loading || append is LoadState.Error) item(span = { GridItemSpan(maxLineSpan) }) {
                    BrowseLoadFooter(append, items::retry)
                }
            }
            BrowseLoadOverlay(refresh, items.itemCount, items::retry, filtered, onResetFilters, emptyIcon)
            if (indexed) BrowseFastScroll(letters, labels, state.firstVisibleItemIndex,
                Modifier.align(Alignment.CenterEnd)) { letter -> scope.launch {
                    val position = labels.firstOrNull { it.title == letter }?.index ?: onLetterPosition(letter)
                    position?.let { state.scrollToItem(it.toInt()) }
                } }
            }
        } else {
            val state = rememberLazyListState()
            LaunchedEffect(state) {
                snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
                    .distinctUntilChanged().collect(onScrolled)
            }
            Box(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().padding(end = indexInset)) {
                if (fields.isNotEmpty() && items.itemSnapshotList.items.any { it.row.fields.isNotEmpty() && it.displaySettings?.showHeader != false })
                    FieldHeader(fields, displaySize)
            LazyColumn(Modifier.fillMaxWidth().weight(1f), state = state, contentPadding = PaddingValues(bottom = 8.dp + LocalPlayerBottomPadding.current)) {
                items(items.itemCount, key = items.itemKey(::browseItemKey)) { index ->
                    val item = items[index]
                    if (item == null) Spacer(Modifier.fillMaxWidth().height(60.dp))
                    else if (item.row.row is LibraryRow.AlbumHeading) HomeShowcase(item, player, onActivate, onMenu, onFavorite,
                        { shuffled -> onPlayItem(item, shuffled) }, onRadio)
                    else if (item.row.fields.isNotEmpty()) {
                        // A list without a header (search) shows each row's own fields.
                        val columns = fields.ifEmpty { item.row.fields.map { AndroidSortField(it.id, "") } }
                        FieldRow(item.copy(displaySettings = item.displaySettings?.let { it.copy(settings = it.settings.copy(display = it.settings.display.copy(size = displaySize))) }), player,
                            columns.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }, columns,
                            onActivate, onMenu, onFavorite, onAddToPlaylist)
                    }
                    else BrowseTrackRow(item, player, onActivate, onMenu, onFavorite, onAddToPlaylist, displaySize)
                }
                if (append is LoadState.Loading || append is LoadState.Error) item { BrowseLoadFooter(append, items::retry) }
            }
            }
            BrowseLoadOverlay(refresh, items.itemCount, items::retry, filtered, onResetFilters, emptyIcon)
            if (indexed) BrowseFastScroll(letters, labels, state.firstVisibleItemIndex,
                Modifier.align(Alignment.CenterEnd)) { letter -> scope.launch {
                    val position = labels.firstOrNull { it.title == letter }?.index ?: onLetterPosition(letter)
                    position?.let { state.scrollToItem(it.toInt()) }
                } }
            }
        }
    }
}


@Composable
private fun BrowseLoadFooter(state: LoadState, retry: () -> Unit) {
    if (state is LoadState.Error) {
        Column(Modifier.fillMaxWidth().padding(16.dp)) {
            io.github.screwys.rufin.ui.ErrorNotice(state.error)
            TextButton(retry) { Text(translate("Retry")) }
        }
    } else if (state is LoadState.Loading) {
        Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) {
            CircularProgressIndicator(Modifier.semantics { contentDescription = translate("Loading...") })
        }
    }
}

@Composable
private fun BoxScope.BrowseLoadOverlay(refresh: LoadState, count: Int, retry: () -> Unit,
    filtered: Boolean, resetFilters: () -> Unit, emptyIcon: String?,
) {
    if (refresh is LoadState.Error) {
        Surface(Modifier.align(if (count == 0) Alignment.Center else Alignment.TopCenter).fillMaxWidth(),
            color = MaterialTheme.colorScheme.surface) { BrowseLoadFooter(refresh, retry) }
    } else if (count == 0) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            if (refresh is LoadState.Loading) CircularProgressIndicator(Modifier.semantics { contentDescription = translate("Loading...") })
            else BrowseEmptyState(filtered, emptyIcon, resetFilters)
        }
    }
}

@Composable
internal fun BrowseEmptyState(filtered: Boolean = false, icon: String? = null, resetFilters: () -> Unit = {}) {
    Column(Modifier.fillMaxWidth().padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp)) {
        if (icon == null) FallbackCover(Modifier.size(64.dp).clip(CoverShape))
        else RufinIcon(icon, null, Modifier.size(64.dp), MaterialTheme.colorScheme.onSurfaceVariant)
        Text(translate(if (filtered) "No results" else "Nothing here yet"),
            style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        if (filtered) TextButton(resetFilters) {
            RufinIcon("rufin-filter-symbolic", null, Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text(translate("Reset"))
        }
    }
}

/** The fast scroller: a track the pane's height with the list's letters, a thumb where the list is
 *  that widens while held, and the held letter in a bubble beside it. */
@Composable
private fun BrowseFastScroll(letters: List<String>, sections: List<AndroidScrollSection>, firstVisible: Int,
    modifier: Modifier, onRelease: (String) -> Unit,
) {
    val release by rememberUpdatedState(onRelease)
    var touched by remember(letters) { mutableStateOf<Int?>(null) }
    var height by remember { mutableIntStateOf(0) }
    val current = sections.lastOrNull { it.index <= firstVisible.toULong() }?.title
    val active = touched ?: current?.let { letters.indexOf(it) } ?: -1
    val density = LocalDensity.current
    val style = MaterialTheme.typography.labelSmall
    val measurer = rememberTextMeasurer()
    val sizes = remember(letters, style, measurer, density.density, density.fontScale) {
        letters.map { measurer.measure(AnnotatedString(it), style, maxLines = 1).size }
    }
    val labelHeight = sizes.maxOf { it.height }.coerceAtLeast(1).toFloat()
    val scale = if (height > 0) minOf(1f, height / (labelHeight * letters.size)) else 1f
    val fittedStyle = style.copy(fontSize = style.fontSize * scale, lineHeight = style.lineHeight * scale)
    val railWidth = with(density) { sizes.maxOf { it.width }.toDp() + 4.dp }.coerceAtLeast(22.dp)
    // Only the letters show; the list's place is the highlighted one, and a held letter shows its bubble.
    val held = touched != null
    val progress = ((active + .5f) / letters.size).coerceIn(0f, 1f)
    Box(modifier.fillMaxHeight().width(railWidth).padding(vertical = 8.dp)
        .onSizeChanged { height = it.height }
        .pointerInput(letters) {
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false)
                fun preview(y: Float) {
                    touched = ((y / size.height.coerceAtLeast(1)) * letters.size).toInt().coerceIn(0, letters.lastIndex)
                }
                preview(down.position.y)
                down.consume()
                try {
                    while (true) {
                        val change = awaitPointerEvent().changes.firstOrNull { it.id == down.id } ?: break
                        preview(change.position.y)
                        change.consume()
                        if (!change.pressed) {
                            touched?.let { release(letters[it]) }
                            break
                        }
                    }
                } finally { touched = null }
            }
        }) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
            letters.forEachIndexed { index, letter ->
                // Each letter is its own accessibility node: a widget takes at most 32 custom actions.
                Box(Modifier.weight(1f).fillMaxWidth().clearAndSetSemantics {
                    contentDescription = letter
                    onClick { release(letter); true }
                }, contentAlignment = Alignment.Center) {
                    Text(letter, style = fittedStyle, maxLines = 1,
                        fontWeight = if (index == active) FontWeight.Bold else FontWeight.Normal,
                        color = if (index == active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
    if (held) Box(modifier.fillMaxHeight().padding(vertical = 8.dp)) {
        val travel = with(density) { (height.toDp() - 48.dp).coerceAtLeast(0.dp) }
        Surface(Modifier.offset(x = -(railWidth + 52.dp), y = travel * progress).size(48.dp),
            shape = androidx.compose.foundation.shape.CircleShape, color = MaterialTheme.colorScheme.primaryContainer,
            shadowElevation = 4.dp) {
            Box(contentAlignment = Alignment.Center) { Text(letters[active], style = MaterialTheme.typography.titleLarge) }
        }
    }
}

/** The height of a transparent bar drawn over a page's band: Home's and the detail pages'. */
internal val LocalOverlayBarHeight = compositionLocalOf { 0.dp }

@Composable
private fun HomeContent(
    sections: List<HomeSectionItems>, player: PlayerConnection, onActivate: (BrowseItem) -> Unit,
    onMenu: (BrowseItem) -> Unit, onPlay: (HomeSectionItems, Boolean) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onScrolled: (Boolean) -> Unit,
    onRadio: (BrowseItem) -> Unit, onStep: suspend (Boolean) -> List<AndroidBrowseRow>,
) {
    val state = rememberLazyListState()
    val sectionScroll = remember {
        object : NestedScrollConnection {
            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource) = Offset(available.x, 0f)
            override suspend fun onPostFling(consumed: Velocity, available: Velocity) = Velocity(available.x, 0f)
        }
    }
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
            .distinctUntilChanged().collect(onScrolled)
    }
    val bar = LocalOverlayBarHeight.current
    val showcase = sections.firstOrNull { it.kind == AndroidHomeSectionKind.SHOWCASE }
    LazyColumn(Modifier.fillMaxSize(), state = state,
        contentPadding = PaddingValues(top = if (showcase == null) bar + 8.dp else 0.dp, bottom = 8.dp + LocalPlayerBottomPadding.current),
        verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(sections, key = { it.index.toString() + it.title }) { section ->
            if (section.kind == AndroidHomeSectionKind.SHOWCASE) {
                HomeBand(section, player, onMenu, onFavorite, { shuffled -> onPlay(section, shuffled) }, onRadio, onStep)
            } else {
                Column {
                    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        // The title, then Play and Shuffle beside it; no refresh.
                        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                            Text(section.title, Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                            ShowcaseAction("rufin-media-playback-start-symbolic", "Play", { onPlay(section, false) }, { onPlay(section, true) })
                            ShowcaseAction("rufin-shuffle-symbolic", "Shuffle", { onPlay(section, true) })
                        }
                    }
                    // Genres wrap as chips, track blocks page through columns of two rows, and the rest
                    // are cards.
                    // Tracks and genres page through columns of two rows; a genre row opens the genre, a
                    // track row its menu, as Home's cards do.
                    if (section.kind == AndroidHomeSectionKind.GENRES || section.kind == AndroidHomeSectionKind.TRACKS) LazyRow(
                        modifier = Modifier.nestedScroll(sectionScroll),
                        contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                        val genres = section.kind == AndroidHomeSectionKind.GENRES
                        items(section.items.chunked(2), key = { column -> browseItemKey(column.first()) }) { column ->
                            Column(Modifier.width(300.dp)) {
                                column.forEach { item ->
                                    if (genres) HomeRow(item, player, { onActivate(item) }, onMenu,
                                        item.row.fields.map { it.text }.filter(String::isNotBlank).joinToString(" · "))
                                    else HomeRow(item, player, { onMenu(it) }, onMenu, item.row.subtitle)
                                }
                            }
                        }
                    } else LazyRow(modifier = Modifier.nestedScroll(sectionScroll),
                        contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        items(section.items, key = ::browseItemKey) { item ->
                            CoverCard(item, player, Modifier.width(150.dp), onMenu, onMenu, oneLine = true)
                        }
                    }
                }
            }
        }
    }
}

/** A row in a Home block (a track or a genre): cover, title and its line, and ⋯ for its menu. */
@Composable
private fun HomeRow(item: BrowseItem, player: PlayerConnection, onClick: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    line: String) {
    val shown = favoriteItem(player.model, item)
    val artwork = rememberBrowseArtwork(item, player, 56.dp)
    Row(Modifier.fillMaxWidth().height(72.dp).clip(RoundedCornerShape(8.dp)).clickable { onClick(shown) },
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        BrowseCover(item, artwork, Modifier.size(56.dp).clip(CoverShape))
        Column(Modifier.weight(1f)) {
            BrowseTitle(shown, maxLines = 1, style = MaterialTheme.typography.bodyLarge, player = player, showDownloadBadge = false)
            if (line.isNotBlank()) Text(line, maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        PlayerIconButton("rufin-view-more-symbolic", "More actions", { onMenu(shown) })
    }
}

/** Home's showcase: Rufin's band full-bleed under the bar, the cover over its edge, the title,
 *  artist and facts on the band and the buttons beside the cover. A swipe shows the album before
 *  or after, which core then steps to; a tap on the cover opens its menu, as on every Home entry. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun HomeBand(section: HomeSectionItems, player: PlayerConnection, onMenu: (BrowseItem) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onPlay: (Boolean) -> Unit, onRadio: (BrowseItem) -> Unit,
    onStep: suspend (Boolean) -> List<AndroidBrowseRow>,
) {
    // The album before, the album shown and the album after; a step answers the next three.
    var shown by remember { mutableStateOf(section.items) }
    LaunchedEffect(section.items) { shown = section.items }
    if (shown.size < 3) return
    val view = shown[1].view
    val pager = androidx.compose.foundation.pager.rememberPagerState(1) { 3 }
    val scope = rememberCoroutineScope()
    LaunchedEffect(pager) {
        snapshotFlow { pager.settledPage }.collect { settled ->
            if (settled == 1) return@collect
            val rows = runCatching { onStep(settled == 2) }.getOrNull().orEmpty()
            if (rows.size == 3) shown = rows.mapIndexed { index, row -> BrowseItem(row, index.toULong(), view, contextTitle = section.title) }
            pager.scrollToPage(1)
        }
    }
    androidx.compose.foundation.pager.HorizontalPager(pager, Modifier.fillMaxWidth()
        .onPreviewKeyEvent { event ->
            // D-pad left and right step the showcase where there is no swipe.
            if (event.type != androidx.compose.ui.input.key.KeyEventType.KeyDown) return@onPreviewKeyEvent false
            val target = when (event.key) {
                androidx.compose.ui.input.key.Key.DirectionLeft -> 0
                androidx.compose.ui.input.key.Key.DirectionRight -> 2
                else -> return@onPreviewKeyEvent false
            }
            scope.launch { pager.animateScrollToPage(target) }
            true
        }, key = { shown[it].row.key + it }) { page ->
        HomeBandPage(shown[page], player, onMenu, onFavorite, onPlay, onRadio, page == 1)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun HomeBandPage(item: BrowseItem, player: PlayerConnection, onMenu: (BrowseItem) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onPlay: (Boolean) -> Unit, onRadio: (BrowseItem) -> Unit,
    /** The album the band shows now, not one a swipe brings in beside it. */
    shown: Boolean,
) {
    val projected = favoriteItem(player.model, item)
    val bar = LocalOverlayBarHeight.current
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        // Phones: a 120 dp cover; wider panes grow it to 196 dp, as GTK's Home does.
        val cover = ((maxWidth - 32.dp) * .3f).coerceIn(112.dp, 196.dp)
        val artwork = rememberBrowseArtwork(item, player, cover)
        // The bar over the band draws this band, at its place and size, once the page moves under it.
        var bandSize by remember { mutableStateOf(Size.Zero) }
        val bandSlot = LocalHomeBand.current
        if (shown) SideEffect { bandSlot?.value = HomeBandLook(artwork.image?.band, bandSize) }
        Column(Modifier.fillMaxWidth()) {
            // The band reaches past the cover's middle, or further when the title, artist and facts need it
            // (a large font), so they always end just above its edge; one layout pass decides it.
            Box(Modifier.fillMaxWidth().onSizeChanged { bandSize = it.toSize() }.then(showcaseBand(artwork.image?.band))) {
                Column(Modifier.fillMaxWidth().padding(start = 16.dp + cover + 16.dp, end = 16.dp, top = bar + 24.dp, bottom = 12.dp)
                    .heightIn(min = cover * .9f - 20.dp), verticalArrangement = Arrangement.Bottom) {
                    CompositionLocalProvider(LocalContentColor provides OnBand) {
                    // Showcases carry no download badge; rows and cards do.
                    BrowseTitle(projected, Modifier.fillMaxWidth(), style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.ExtraBold),
                        showDownloadBadge = false,
                        marquee = true)
                    if (item.row.subtitle.isNotBlank()) Text(item.row.subtitle, Modifier.rufinMarquee().padding(top = 2.dp),
                        style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.SemiBold, maxLines = 1)
                    FactsPill(listOfNotNull(item.row.year?.let { Triple("rufin-x-office-calendar-symbolic", "Year", it.toString()) },
                        Triple("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount)),
                        Triple("rufin-preferences-system-time-symbolic", "Duration", item.row.durationText)), Modifier.padding(top = 8.dp))
                    }
                }
            }
            // The buttons start under the band's edge, beside the cover.
            FlowRow(Modifier.padding(start = 16.dp + cover + 16.dp, end = 16.dp, top = 12.dp, bottom = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                ShowcasePill("rufin-media-playback-start-symbolic", "Play", true, { onPlay(false) }, { onPlay(true) })
                ShowcasePill("rufin-audio-only-symbolic", "Radio", false, { onRadio(item) })
                ShowcaseRound(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", "Favorite",
                    { onFavorite(projected) }, if (projected.row.favorite) MaterialTheme.colorScheme.error else LocalContentColor.current)
            }
        }
        BrowseCover(item, artwork, Modifier.padding(start = 16.dp, top = bar + 24.dp, bottom = 16.dp).size(cover)
            .shadow(14.dp, CoverShape, ambientColor = Color.Black.copy(alpha = .4f), spotColor = Color.Black.copy(alpha = .4f)).clip(CoverShape)
            .combinedClickable(onClick = { onMenu(projected) }, onLongClick = { onMenu(projected) }))
    }
}

/** Play (hold to shuffle) in the accent, and Radio in the tonal box Favorite, Download and ⋯ share. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun ShowcasePill(icon: String, label: String, primary: Boolean, onClick: () -> Unit, onLongClick: (() -> Unit)? = null) {
    val container = if (primary) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface.copy(alpha = .11f)
    val content = if (primary) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface
    Row(Modifier.height(40.dp).clip(RoundedCornerShape(20.dp)).background(container)
        .combinedClickable(onClick = onClick, onLongClick = onLongClick,
            onLongClickLabel = onLongClick?.let { translate("{action} (Shuffle)").replace("{action}", translate(label)) })
        .padding(start = 14.dp, end = 18.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        RufinIcon(icon, null, Modifier.size(16.dp), content)
        Text(translate(label), color = content, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.Bold)
    }
}

@Composable
internal fun ShowcaseRound(icon: String, label: String, onClick: () -> Unit, tint: Color = LocalContentColor.current) {
    Box(Modifier.size(40.dp).clip(androidx.compose.foundation.shape.CircleShape)
        .background(MaterialTheme.colorScheme.onSurface.copy(alpha = .11f)).clickable(onClick = onClick),
        contentAlignment = Alignment.Center) {
        RufinIcon(icon, translate(label), Modifier.size(20.dp), tint)
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun ShowcaseAction(icon: String, label: String, action: () -> Unit, shuffle: (() -> Unit)? = null,
    tint: Color = LocalContentColor.current,
) {
    Box(Modifier.size(48.dp).combinedClickable(onClick = action, onLongClick = shuffle,
        onLongClickLabel = if (shuffle == null) null else translate("{action} (Shuffle)").replace("{action}", translate(label))), contentAlignment = Alignment.Center) {
        RufinIcon(icon, translate(label), Modifier.size(24.dp), tint, glyph = true)
    }
}

private fun browseItemKey(item: BrowseItem): String = "${item.row.sourceId.orEmpty()}:${item.row.key}:${item.index}"

/** The item showing the favorites, ratings and downloads committed since it was read. */
@Composable
internal fun favoriteItem(model: io.github.screwys.rufin.app.RufinConnection, item: BrowseItem): BrowseItem =
    model.browse.shown(item)

@Composable
internal fun BrowseTitle(item: BrowseItem, modifier: Modifier = Modifier,
    maxLines: Int = 1, style: androidx.compose.ui.text.TextStyle = MaterialTheme.typography.bodyLarge,
    player: PlayerConnection? = null, marquee: Boolean = false, showDownloadBadge: Boolean = true,
    title: String = item.row.title) {
    val row = item.row
    val occurrence = row.track?.entry != null
    val track = row.isTrack
    val playing by remember(track, row.mediaUri, row.contextId, occurrence, item.index, item.view, player) { derivedStateOf {
        player != null && player.playingMediaUri != null &&
            if (track) {
                player.playingMediaUri == row.mediaUri && (!occurrence ||
                    player.currentRow?.let { item.view?.playingPosition(it) } == item.index)
            } else {
                val target = row.contextId
                val context = player.currentContextId
                target != null && context != null && playsContext(target, context)
            }
    } }
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        if (row.playlistFolder != null) RufinIcon("rufin-folders-symbolic", null, Modifier.size(18.dp))
        if (playing) PlayingIndicator()
        Text(title, Modifier.weight(1f, fill = false).then(if (marquee) Modifier.rufinMarquee() else Modifier),
            maxLines = maxLines, overflow = TextOverflow.Ellipsis, style = style,
            color = if (playing) MaterialTheme.colorScheme.secondary else LocalContentColor.current)
        if (showDownloadBadge) io.github.screwys.rufin.ui.DownloadBadge(item)
    }
}

// Placeholder cards' palettes, picked by a hash of the item's key.
private val showcasePalettes = arrayOf(
    intArrayOf(176,72,88, 93,60,151, 196,129,73),
    intArrayOf(194,116,57, 129,72,53, 208,165,79),
    intArrayOf(143,147,61, 85,113,72, 185,156,71),
    intArrayOf(65,150,98, 53,108,103, 126,176,92),
    intArrayOf(55,150,145, 55,102,143, 90,181,159),
    intArrayOf(58,135,184, 62,87,151, 89,180,195),
    intArrayOf(72,103,187, 74,66,151, 98,143,211),
    intArrayOf(116,82,188, 83,68,143, 155,111,210),
    intArrayOf(156,74,177, 95,66,145, 186,104,190),
    intArrayOf(190,71,139, 117,61,126, 207,112,157),
    intArrayOf(203,83,104, 129,61,89, 217,130,119),
    intArrayOf(210,101,72, 143,70,64, 221,151,105),
    intArrayOf(165,119,73, 104,77,65, 198,158,99),
    intArrayOf(104,147,93, 66,105,81, 155,178,112),
    intArrayOf(93,134,153, 65,91,126, 126,165,176),
    intArrayOf(92,89,170, 70,68,128, 135,126,197),
)

private fun showcaseMix(first: Color, second: Color, weight: Float): Color {
    val firstAlpha = first.alpha * weight
    val secondAlpha = second.alpha * (1f - weight)
    val alpha = firstAlpha + secondAlpha
    return Color((first.red * firstAlpha + second.red * secondAlpha) / alpha,
        (first.green * firstAlpha + second.green * secondAlpha) / alpha,
        (first.blue * firstAlpha + second.blue * secondAlpha) / alpha, alpha)
}

/** A placeholder card's three stops for `key`. */
@Composable
private fun showcaseColors(key: String): List<Color> {
    val scheme = MaterialTheme.colorScheme
    return remember(key, scheme.background, scheme.surfaceContainer) {
        val seed = key.toByteArray(Charsets.UTF_8).fold(0x811c9dc5u) { hash, byte -> hash * 16777619u xor byte.toUByte().toUInt() }
        val palette = showcasePalettes[((seed xor seed.rotateLeft(13) xor seed.rotateRight(9)) % 16u).toInt()]
        fun color(offset: Int, alpha: Float) = Color(palette[offset] / 255f, palette[offset + 1] / 255f, palette[offset + 2] / 255f, alpha)
        listOf(showcaseMix(color(0, .78f), scheme.background, .58f).compositeOver(scheme.background),
            showcaseMix(color(3, .64f), scheme.surfaceContainer, .44f).compositeOver(scheme.background),
            showcaseMix(scheme.background, color(6, .56f), .62f).compositeOver(scheme.background))
    }
}

/** A page's band: its cover's colours (`PlayerArtwork.band`) at 135°, as CSS places them,
 *  darker toward the edge the cover overlaps; a neutral dark until the cover has loaded. */
internal fun showcaseBand(stops: List<Color>?): Modifier = Modifier.drawBehind { drawBand(stops, size) }

/** The band of `stops` as a band of `area` draws it from this layout's top-left corner. */
internal fun DrawScope.drawBand(stops: List<Color>?, area: Size) {
    val colors = stops ?: neutralBand
    val reach = (area.width + area.height) / 4f
    val centre = Offset(area.width / 2f, area.height / 2f)
    drawRect(Brush.linearGradient(0f to colors[0], .58f to colors[1], 1f to colors[2],
        start = centre - Offset(reach, reach), end = centre + Offset(reach, reach)))
    drawRect(Brush.verticalGradient(.3f to Color.Transparent, 1f to Color.Black.copy(alpha = .3f), startY = 0f, endY = area.height))
}

/** Home's showcase band as the bar over it draws it: its colours and its size. */
internal class HomeBandLook(val stops: List<Color>?, val size: Size)

/** Where Home's showcase tells the bar over it how its band looks. */
internal val LocalHomeBand = compositionLocalOf<MutableState<HomeBandLook?>?> { null }

private val neutralBand = listOf(Color(0xFF2D3036), Color(0xFF282A30), Color(0xFF212328))

/** Text and icons on a band, which is always dark. */
internal val OnBand = Color.White.copy(alpha = .92f)

/** The middle colour of the band a detail page's header shows, for its bar once scrolled. */
internal val LocalDetailBand = compositionLocalOf<MutableState<Color?>?> { null }

@Composable
internal fun showcasePaint(key: String): Modifier {
    val scheme = MaterialTheme.colorScheme
    val colors = showcaseColors(key)
    return Modifier.shadow(4.dp, RoundedCornerShape(8.dp), ambientColor = Color.Black.copy(alpha = .08f),
        spotColor = Color.Black.copy(alpha = .08f)).clip(RoundedCornerShape(8.dp)).drawWithCache {
        val center = Offset(size.width / 2f, size.height / 2f)
        val extent = (size.width + size.height) / 4f
        val brush = Brush.linearGradient(0f to colors[0], .58f to colors[1], 1f to colors[2],
            start = center - Offset(extent, extent), end = center + Offset(extent, extent))
        onDrawBehind { drawRect(brush) }
    }.border(1.dp, scheme.onBackground.copy(alpha = .1f), RoundedCornerShape(8.dp))
}

@Composable
private fun rememberBrowseArtwork(item: BrowseItem, player: PlayerConnection, size: Dp): Cover {
    val pixels = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
    return rememberArtwork(item.row.artworkIdentity, player, pixels)
}

@Composable
private fun BrowseCover(item: BrowseItem, artwork: Cover, modifier: Modifier) {
    Box(modifier.clip(CoverShape).background(MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        if (artwork.missing && artwork.image == null && item.row.isFolder) RufinIcon("rufin-folders-symbolic", null, Modifier.size(24.dp))
        else Artwork(artwork, Modifier.fillMaxSize())
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun CoverCard(item: BrowseItem, player: PlayerConnection, modifier: Modifier,
    onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    /** Home's cards: a line each for the title and its labels, cut short rather than sliding, as only the showcase slides. */
    oneLine: Boolean = false,
) {
    val shown = favoriteItem(player.model, item)
    BoxWithConstraints(modifier) {
        val artwork = rememberBrowseArtwork(item, player, maxWidth)
        BrowseSwipeActions(item, !oneLine) {
        Column(Modifier.fillMaxWidth().playlistFolderDrop(item, player.model).combinedClickable(onClick = { onActivate(item) },
            onLongClick = { onMenu(item) }),
            verticalArrangement = Arrangement.spacedBy(0.dp)) {
            Box(Modifier.fillMaxWidth().aspectRatio(1f)) {
                BrowseCover(item, artwork, Modifier.fillMaxSize())
            }
            Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                // Cards carry no download badge; list rows do.
                BrowseTitle(shown, Modifier.weight(1f), maxLines = if (oneLine) 1 else 2,
                    style = MaterialTheme.typography.titleSmall, player = player, showDownloadBadge = false)
                item.row.playlistFolderMember?.let { member -> PlaylistDragHandle(member, Modifier.size(32.dp)) }
            }
            // The labels under the title: the list's grid fields, one per line.
            shown.row.fields.map { it.text }.filter(String::isNotBlank).forEach { line ->
                Text(line, Modifier.fillMaxWidth(), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1,
                    overflow = TextOverflow.Ellipsis)
            }
        }
        }
    }
}

@Composable
private fun CoverPlaceholder() {
    Column {
        Box(Modifier.fillMaxWidth().aspectRatio(1f).clip(CoverShape).background(MaterialTheme.colorScheme.surfaceContainerHighest))
        Spacer(Modifier.height(60.dp))
    }
}

@OptIn(ExperimentalFoundationApi::class, ExperimentalLayoutApi::class)
@Composable
private fun HomeShowcase(item: BrowseItem, player: PlayerConnection, onActivate: (BrowseItem) -> Unit,
    onMenu: (BrowseItem) -> Unit, onFavorite: (BrowseItem) -> Unit, onPlay: (Boolean) -> Unit,
    onRadio: (BrowseItem) -> Unit, section: String = "",
) {
    val projected = favoriteItem(player.model, item)
    BoxWithConstraints(Modifier.padding(horizontal = 16.dp).fillMaxWidth()) {
        val coverSize = ((maxWidth - 20.dp) * .35f).coerceIn(96.dp, 180.dp)
        val artwork = rememberBrowseArtwork(item, player, coverSize)
        Surface(Modifier.fillMaxWidth().then(showcasePaint(item.row.key)).combinedClickable(onClick = { onActivate(item) }, onLongClick = { onMenu(projected) }), shape = RoundedCornerShape(8.dp),
            color = androidx.compose.ui.graphics.Color.Transparent, contentColor = MaterialTheme.colorScheme.onSurface) {
        Box(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                BrowseCover(item, artwork, Modifier.size(coverSize).shadow(6.dp, CoverShape,
                    ambientColor = Color.Black.copy(alpha = .18f), spotColor = Color.Black.copy(alpha = .18f)).clip(CoverShape))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (section.isNotBlank()) Text(section.uppercase(), color = MaterialTheme.colorScheme.primary,
                            style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.Bold)
                        CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                            PlayerIconButton("rufin-audio-only-symbolic", "Play radio", { onRadio(item) }, Modifier.size(32.dp), iconSize = 16.dp)
                        }
                    }
                    BrowseTitle(projected, Modifier.fillMaxWidth(), style = MaterialTheme.typography.titleLarge, marquee = true, showDownloadBadge = false)
                    Text(item.row.subtitle, Modifier.rufinMarquee(), style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        item.row.year?.let { SummaryFact("rufin-x-office-calendar-symbolic", "Year", it.toString()) }
                        SummaryFact("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount))
                        SummaryFact("rufin-preferences-system-time-symbolic", "Duration", item.row.durationText)
                    }
                    FlowRow {
                        ShowcaseAction("rufin-media-playback-start-symbolic", "Play album", { onPlay(false) }, { onPlay(true) })
                        ShowcaseAction(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                            "Favorite", { onFavorite(projected) }, tint = if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)

                    }
                }
            }
        }
    }
    }
}

/** Year, tracks, duration and the like in a pill, divided by |: the showcase's and the detail headers' one
 *  look. On the band it is dark so it reads on any cover; on the page it is a light tint of the page's text.
 *  Each fact is (icon, label, value). */
@Composable
internal fun FactsPill(facts: List<Triple<String, String, String>>, modifier: Modifier = Modifier, onBand: Boolean = true) {
    val text = if (onBand) OnBand else MaterialTheme.colorScheme.onSurface
    Surface(modifier, shape = RoundedCornerShape(50), color = if (onBand) Color.Black.copy(alpha = .28f) else text.copy(alpha = .1f), contentColor = text) {
        // The facts slide inside the pill when they are wider than the room.
        Row(Modifier.padding(horizontal = 10.dp, vertical = 4.dp).rufinMarquee(), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            facts.forEachIndexed { index, (icon, label, value) ->
                if (index > 0) Text("|", style = MaterialTheme.typography.labelSmall, color = text.copy(alpha = .45f))
                SummaryFact(icon, label, value)
            }
        }
    }
}

@Composable
internal fun SummaryFact(icon: String, label: String, value: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.semantics { contentDescription = "${translate(label)} $value" }) {
        RufinIcon(icon, null, Modifier.size(14.dp))
        Text(value, style = MaterialTheme.typography.labelSmall)
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun BrowseTrackRow(item: BrowseItem, player: PlayerConnection,
    onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onAddToPlaylist: (BrowseItem) -> Unit, displaySize: DisplaySize,
) {
    val imageSize = when (displaySize) { DisplaySize.COMPACT -> 36.dp; DisplaySize.LARGE -> 64.dp; else -> 50.dp }
    val rowPadding = if (displaySize == DisplaySize.COMPACT) 0.dp else 6.dp
    val artwork = rememberBrowseArtwork(item, player, imageSize)
    BrowseSwipeActions(item, true) {
    Row(Modifier.fillMaxWidth().heightIn(min = if (displaySize == DisplaySize.COMPACT) 48.dp else 64.dp)
        .playlistFolderDrop(item, player.model)
        .combinedClickable(onClick = { onActivate(item) }, onLongClick = { onMenu(item) }).padding(start = 16.dp, end = 8.dp, top = rowPadding, bottom = rowPadding),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        BrowseCover(item, artwork, Modifier.size(imageSize))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            BrowseTitle(item, player = player)
            Text(item.row.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            item.row.lyricsSnippet?.let { LyricsSnippetText(it) }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
                if (item.row.isTrack && item.row.durationMillis > 0UL) Text(item.row.durationText,
                    style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                item.row.playlistFolderMember?.let { member -> PlaylistDragHandle(member) }
                    ?: PlayerIconButton("rufin-view-more-symbolic", "More", { onMenu(item) }, Modifier.size(36.dp), iconSize = 20.dp)
        }
    }
    }
}

/** The lyrics lines a search by lyrics matched, the match highlighted. */
@Composable
internal fun LyricsSnippetText(snippet: LyricsSnippet) {
    val accent = MaterialTheme.colorScheme.primary
    Text(buildAnnotatedString {
        snippet.parts.forEach { part ->
            if (part.highlighted) withStyle(SpanStyle(color = accent, fontWeight = FontWeight.Bold)) { append(part.text) }
            else append(part.text)
        }
    }, maxLines = 2, overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}
