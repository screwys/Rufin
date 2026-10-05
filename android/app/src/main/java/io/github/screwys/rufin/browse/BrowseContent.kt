package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.Artwork
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
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.AnnotatedString
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
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.collect

@Composable
internal fun BrowseContent(
    items: LazyPagingItems<BrowseItem>, homeRows: List<BrowseItem>, player: PlayerConnection,
    home: Boolean, detail: Boolean, covers: Boolean, onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    onSectionPlay: (String, Boolean) -> Unit, onRefreshSection: (String) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onAddToPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit, scrollSections: List<AndroidScrollSection> = emptyList(),
    displaySize: String = "Default", gridSpacing: String = "Default", onPlayItem: (BrowseItem, Boolean) -> Unit = { item, _ -> onActivate(item) },
    detailFields: List<AndroidSortField> = emptyList(),
    letterIndex: Boolean = false,
    onLetterPosition: suspend (String) -> ULong? = { null },
    letterDescending: Boolean = false,
    filtered: Boolean = false, onResetFilters: () -> Unit = {},
    loading: Boolean = false,
    onRadio: (BrowseItem) -> Unit,
    emptyIcon: String? = null,
) {
    val coverWidth = when (displaySize) { "Compact" -> 104.dp; "Large" -> 176.dp; else -> 128.dp }
    val spacing = when (gridSpacing) { "Small" -> 4.dp; "Wide" -> 16.dp; else -> 12.dp }
    if (home) {
        HomeContent(homeRows, player, onActivate, onMenu, onSectionPlay, onRefreshSection, onFavorite, onScrolled, onRadio)
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
            LazyVerticalGrid(GridCells.Adaptive(coverWidth), Modifier.fillMaxSize().padding(end = indexInset), state = state,
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
                if (detailFields.isNotEmpty() && items.itemSnapshotList.items.any { it.row.kind == "track" && it.displaySettings?.showHeader != false })
                    DetailTrackHeader(detailFields, displaySize)
            LazyColumn(Modifier.fillMaxWidth().weight(1f), state = state, contentPadding = PaddingValues(bottom = 8.dp + LocalPlayerBottomPadding.current)) {
                items(items.itemCount, key = items.itemKey(::browseItemKey)) { index ->
                    val item = items[index]
                    if (item == null) Spacer(Modifier.fillMaxWidth().height(60.dp))
                    else if (item.row.kind == "album_header") HomeShowcase(item, player, onActivate, onMenu, onFavorite,
                        { _, shuffled -> onPlayItem(item, shuffled) }, onRadio)
                    else if (detailFields.isNotEmpty() && item.row.kind == "track")
                        DetailTrackRow(item.copy(displaySettings = item.displaySettings?.copy(size = displaySize)), player,
                            detailFields.any { it.id == "Image" || it.id == "TitleMerged" }, detailFields,
                            onActivate, onMenu, onFavorite, onAddToPlaylist)
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
        if (icon == null) FallbackCover(Modifier.size(64.dp).clip(RoundedCornerShape(12.dp)))
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
    Box(modifier.fillMaxHeight().width(railWidth).background(MaterialTheme.colorScheme.surface.copy(alpha = .9f)).padding(vertical = 8.dp)
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
        }
        .clearAndSetSemantics {
            contentDescription = letters.joinToString(" · ")
            customActions = letters.map { letter -> CustomAccessibilityAction(letter) { release(letter); true } }
        }) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
            letters.forEachIndexed { index, letter ->
                Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                    Text(letter, style = fittedStyle, maxLines = 1,
                        fontWeight = if (index == active) FontWeight.Bold else FontWeight.Normal,
                        color = if (index == active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (touched != null) Surface(Modifier.align(Alignment.CenterStart).offset(x = (-56).dp).size(48.dp),
            shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.primaryContainer) {
            Box(contentAlignment = Alignment.Center) { Text(letters[active], style = MaterialTheme.typography.titleLarge) }
        }
    }
}

@Composable
private fun HomeContent(
    rows: List<BrowseItem>, player: PlayerConnection, onActivate: (BrowseItem) -> Unit,
    onMenu: (BrowseItem) -> Unit, onPlay: (String, Boolean) -> Unit, onRefresh: (String) -> Unit,
    onFavorite: (BrowseItem) -> Unit, onScrolled: (Boolean) -> Unit,
    onRadio: (BrowseItem) -> Unit,
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
    val sections = remember(rows) { rows.groupBy { it.row.sectionId }.values.toList() }
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current),
        verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(sections, key = { it.first().row.sectionId }) { section ->
            val header = section.first().row
            if (header.sectionKind == "showcase") {
                section.forEach { item -> HomeShowcase(item, player, onActivate, onMenu, onFavorite, onPlay, onRadio) }
            } else {
                Column {
                    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                            Text(header.section, Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                            if (header.sectionRefreshable) PlayerIconButton("rufin-view-refresh-symbolic", "Refresh", { onRefresh(header.sectionId) }, glyph = true)
                        }
                        ShowcaseAction("rufin-shuffle-symbolic", "Shuffle", { onPlay(header.sectionId, true) })
                        ShowcaseAction("rufin-media-playback-start-symbolic", "Play", { onPlay(header.sectionId, false) }, { onPlay(header.sectionId, true) })
                    }
                    LazyRow(modifier = Modifier.nestedScroll(sectionScroll),
                        contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        items(section, key = ::browseItemKey) { item ->
                            if (header.sectionKind == "genres") {
                                AssistChip(onClick = { onActivate(item) }, label = { Text(item.row.title) },
                                    leadingIcon = { RufinIcon("rufin-audio-x-generic-symbolic", null, Modifier.size(20.dp)) })
                            } else CoverCard(item, player, Modifier.width(150.dp), onActivate, onMenu, marquee = true)
                        }
                    }
                }
            }
        }
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

@Composable
internal fun favoriteItem(model: io.github.screwys.rufin.app.RufinConnection, item: BrowseItem): BrowseItem {
    val kind = if (item.row.kind == "album_header") "album" else item.row.kind
    if (kind !in setOf("track", "album", "artist")) return item
    var effective by remember(kind, item.row.mediaUri) { mutableStateOf(item.row.favorite) }
    LaunchedEffect(item.row.favorite) { effective = item.row.favorite }
    LaunchedEffect(model, kind, item.row.mediaUri) {
        model.favoriteSettlements.collect { settlement ->
            if (settlement.kind == kind && settlement.mediaUri == item.row.mediaUri) effective = settlement.effective
        }
    }
    return item.copy(row = item.row.copy(favorite = model.projectedFavorite(kind, item.row.mediaUri, effective)))
}

@Composable
internal fun BrowseTitle(row: io.github.screwys.rufin.core.AndroidBrowseRow, modifier: Modifier = Modifier,
    maxLines: Int = 1, style: androidx.compose.ui.text.TextStyle = MaterialTheme.typography.bodyLarge,
    player: PlayerConnection? = null, marquee: Boolean = false, showDownloadBadge: Boolean = true) {
    val playing by remember(row.kind, row.mediaUri, row.playbackContextId, player) { derivedStateOf {
        player?.playback?.let { playback ->
            playback.desiredPlaying && if (row.kind == "track") playback.mediaUri == row.mediaUri else {
                val target = row.playbackContextId
                val context = playback.contextId
                target != null && context != null && (context == target || context.startsWith("$target|query="))
            }
        } == true
    } }
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        if (playing) PlayingIndicator()
        Text(row.title, Modifier.weight(1f, fill = false).then(if (marquee) Modifier.rufinMarquee() else Modifier),
            maxLines = maxLines, overflow = TextOverflow.Ellipsis, style = style,
            color = if (playing) MaterialTheme.colorScheme.secondary else LocalContentColor.current)
        if (showDownloadBadge) io.github.screwys.rufin.ui.DownloadBadge(row)
    }
}

// The palettes, stops and seed match resources/showcase.css and GTK artwork presentation.
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

@Composable
internal fun showcasePaint(key: String): Modifier {
    val scheme = MaterialTheme.colorScheme
    val colors = remember(key, scheme.background, scheme.surfaceContainer) {
        val seed = key.toByteArray(Charsets.UTF_8).fold(0x811c9dc5u) { hash, byte -> hash * 16777619u xor byte.toUByte().toUInt() }
        val palette = showcasePalettes[((seed xor seed.rotateLeft(13) xor seed.rotateRight(9)) % 16u).toInt()]
        fun color(offset: Int, alpha: Float) = Color(palette[offset] / 255f, palette[offset + 1] / 255f, palette[offset + 2] / 255f, alpha)
        listOf(showcaseMix(color(0, .78f), scheme.background, .58f).compositeOver(scheme.background),
            showcaseMix(color(3, .64f), scheme.surfaceContainer, .44f).compositeOver(scheme.background),
            showcaseMix(scheme.background, color(6, .56f), .62f).compositeOver(scheme.background))
    }
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
private fun rememberBrowseArtwork(item: BrowseItem, player: PlayerConnection, size: Dp): PlayerArtwork? {
    val pixels = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
    return rememberArtwork(item.row.artworkIdentity, player, pixels)
}

@Composable
private fun BrowseCover(item: BrowseItem, artwork: PlayerArtwork?, modifier: Modifier) {
    Box(modifier.clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        if (artwork != null) Artwork(artwork, Modifier.fillMaxSize())
        else if (item.row.kind == "folder") RufinIcon("rufin-folders-symbolic", null, Modifier.size(24.dp))
        else FallbackCover(Modifier.fillMaxSize())
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun CoverCard(item: BrowseItem, player: PlayerConnection, modifier: Modifier,
    onActivate: (BrowseItem) -> Unit, onMenu: (BrowseItem) -> Unit,
    marquee: Boolean = false,
) {
    BoxWithConstraints(modifier) {
        val artwork = rememberBrowseArtwork(item, player, maxWidth)
        BrowseSwipeActions(item, !marquee) {
        Column(Modifier.fillMaxWidth().combinedClickable(onClick = { onActivate(item) }, onLongClick = { onMenu(item) }),
            verticalArrangement = Arrangement.spacedBy(0.dp)) {
            Box(Modifier.fillMaxWidth().aspectRatio(1f)) {
                BrowseCover(item, artwork, Modifier.fillMaxSize())
            }
            BrowseTitle(item.row, Modifier.fillMaxWidth().padding(top = 6.dp),
                maxLines = if (marquee) 1 else 2, style = MaterialTheme.typography.titleSmall, player = player, marquee = marquee)
            val subtitle = listOf(item.row.subtitle, item.row.year?.takeIf { item.row.kind != "track" }?.toString().orEmpty())
                .filter(String::isNotBlank).joinToString(" · ")
            if (subtitle.isNotBlank()) Text(subtitle, Modifier.fillMaxWidth().then(if (marquee) Modifier.rufinMarquee() else Modifier), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = if (marquee) 1 else 2,
                overflow = TextOverflow.Ellipsis)
        }
        }
    }
}

@Composable
private fun CoverPlaceholder() {
    Column {
        Box(Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(12.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest))
        Spacer(Modifier.height(60.dp))
    }
}

@OptIn(ExperimentalFoundationApi::class, ExperimentalLayoutApi::class)
@Composable
private fun HomeShowcase(item: BrowseItem, player: PlayerConnection, onActivate: (BrowseItem) -> Unit,
    onMenu: (BrowseItem) -> Unit, onFavorite: (BrowseItem) -> Unit, onPlay: (String, Boolean) -> Unit,
    onRadio: (BrowseItem) -> Unit,
) {
    val projected = favoriteItem(player.model, item)
    BoxWithConstraints(Modifier.padding(horizontal = 16.dp).fillMaxWidth()) {
        val coverSize = ((maxWidth - 20.dp) * .35f).coerceIn(96.dp, 180.dp)
        val artwork = rememberBrowseArtwork(item, player, coverSize)
        Surface(Modifier.fillMaxWidth().then(showcasePaint(item.row.key)).combinedClickable(onClick = { onActivate(item) }, onLongClick = { onMenu(item) }), shape = RoundedCornerShape(8.dp),
            color = androidx.compose.ui.graphics.Color.Transparent, contentColor = MaterialTheme.colorScheme.onSurface) {
        Box(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                BrowseCover(item, artwork, Modifier.size(coverSize).shadow(6.dp, RoundedCornerShape(12.dp),
                    ambientColor = Color.Black.copy(alpha = .18f), spotColor = Color.Black.copy(alpha = .18f)).clip(RoundedCornerShape(12.dp)))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (item.row.section.isNotBlank()) Text(item.row.section.uppercase(), color = MaterialTheme.colorScheme.primary,
                            style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.Bold)
                        CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 32.dp) {
                            PlayerIconButton("rufin-audio-only-symbolic", "Play radio", { onRadio(item) }, Modifier.size(32.dp), iconSize = 16.dp)
                        }
                    }
                    BrowseTitle(item.row, Modifier.fillMaxWidth(), style = MaterialTheme.typography.titleLarge, player = player, marquee = true)
                    Text(item.row.subtitle, Modifier.rufinMarquee(), style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        item.row.year?.let { SummaryFact("rufin-x-office-calendar-symbolic", "Year", it.toString()) }
                        SummaryFact("rufin-tracks-symbolic", "Tracks", trackCountText(item.row.trackCount))
                        SummaryFact("rufin-preferences-system-time-symbolic", "Duration", browseDuration(item.row.durationMillis))
                    }
                    FlowRow {
                        ShowcaseAction("rufin-media-playback-start-symbolic", "Play album", { onPlay(item.row.sectionId, false) }, { onPlay(item.row.sectionId, true) })
                        ShowcaseAction(if (projected.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                            "Favorite", { onFavorite(projected) }, tint = if (projected.row.favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)

                    }
                }
            }
        }
    }
    }
}

@Composable
private fun SummaryFact(icon: String, label: String, value: String) {
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
    onFavorite: (BrowseItem) -> Unit, onAddToPlaylist: (BrowseItem) -> Unit, displaySize: String,
) {
    val imageSize = when (displaySize) { "Compact" -> 36.dp; "Large" -> 64.dp; else -> 50.dp }
    val rowPadding = if (displaySize == "Compact") 0.dp else 6.dp
    val artwork = rememberBrowseArtwork(item, player, imageSize)
    BrowseSwipeActions(item, true) {
    Row(Modifier.fillMaxWidth().heightIn(min = if (displaySize == "Compact") 48.dp else 64.dp)
        .combinedClickable(onClick = { onActivate(item) }, onLongClick = { onMenu(item) }).padding(start = 16.dp, end = 8.dp, top = rowPadding, bottom = rowPadding),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        BrowseCover(item, artwork, Modifier.size(imageSize))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            BrowseTitle(item.row, player = player)
            Text(item.row.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
                if (item.row.kind == "track" && item.row.durationMillis > 0UL) Text(browseDuration(item.row.durationMillis),
                    style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                PlayerIconButton("rufin-view-more-symbolic", "More", { onMenu(item) })
        }
    }
    }
}

private fun browseDuration(millis: ULong): String {
    val seconds = millis / 1000UL
    return if (seconds >= 3600UL) "${seconds / 3600UL}:${((seconds / 60UL) % 60UL).toString().padStart(2, '0')}:${(seconds % 60UL).toString().padStart(2, '0')}"
    else "${seconds / 60UL}:${(seconds % 60UL).toString().padStart(2, '0')}"
}
