package io.github.screwys.rufin.player

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import io.github.screwys.rufin.shared.rufin_core.PreparedQueuePage
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.browse.*
import io.github.screwys.rufin.shared.library.QueueRow
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.browse.BrowseItem
import io.github.screwys.rufin.browse.BrowseMenuSheet
import io.github.screwys.rufin.browse.PlaylistPickerSheet
import io.github.screwys.rufin.browse.PlaylistCreateSheet
import io.github.screwys.rufin.ui.PlayingIndicator
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.metadata.MetadataEditorScreen
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.rememberArtwork
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PlayerQueueSheet(player: PlayerConnection, onDismiss: () -> Unit,
    onNavigate: (Route, String, String?) -> Unit = { route, title, _ -> player.model.browse.openRoute(route, title) },
) {
    // A sheet that opens half-way and pulls up to the full height.
    ModalBottomSheet(onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = false),
        containerColor = MaterialTheme.colorScheme.background) {
        PlayerQueue(player, Modifier.fillMaxSize().navigationBarsPadding(), onActivated = onDismiss, onNavigate = onNavigate)
    }
}

/** The queue: its tools, the playing track pinned on top where no player shows it beside the queue, and the
 *  rows. A sheet on phones; a pane beside the library and in the player on wide screens. */
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PlayerQueue(player: PlayerConnection, modifier: Modifier = Modifier, nowPlaying: Boolean = true,
    /** A row was picked to play. */
    onActivated: () -> Unit = {},
    onNavigate: (Route, String, String?) -> Unit = { route, title, _ -> player.model.browse.openRoute(route, title) },
) {
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    val reduceMotion = LocalReduceMotion.current
    val translation = LocalTranslationRevision.current
    val revision by remember(player) { derivedStateOf { player.playback?.queue?.revision } }
    val output by remember(player) { derivedStateOf { player.playback?.controls?.playbackOutput } }
    val currentIndex by remember(player) { derivedStateOf { player.playback?.queue?.currentIndex } }
    var searchVisible by remember { mutableStateOf(false) }
    // Select: rows a tap picks, which then move up or down together or leave; dragging still works.
    var selecting by remember { mutableStateOf(false) }
    var picked by remember { mutableStateOf(setOf<ULong>()) }
    var search by remember { mutableStateOf("") }
    var filter by remember { mutableStateOf("") }
    var pages by remember { mutableStateOf<Map<Int, PreparedQueuePage>>(emptyMap()) }
    var menu by remember { mutableStateOf<Pair<QueueRow, BrowseItem>?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var metadataEditor by remember { mutableStateOf<BrowseItem?>(null) }
    var create by remember { mutableStateOf(false) }
    var clear by remember { mutableStateOf(false) }
    var dragged by remember { mutableStateOf<ULong?>(null) }
    var dragIndex by remember { mutableIntStateOf(0) }
    var dragOrigin by remember { mutableIntStateOf(0) }
    var dragTop by remember { mutableFloatStateOf(0f) }
    var dragHeight by remember { mutableIntStateOf(0) }
    var dragDistance by remember { mutableFloatStateOf(0f) }
    var dragRow by remember { mutableStateOf<QueueRow?>(null) }
    // The dropped row settles into its place by its index: its position, the rows' key, changes with the move.
    var releasedIndex by remember { mutableStateOf<Int?>(null) }
    // From a drop until the moved queue is shown, rows take their new keys without sliding to them.
    var reordered by remember { mutableStateOf(false) }
    val releaseOffset = remember { Animatable(0f) }
    var dragScroll by remember { mutableFloatStateOf(0f) }
    var initialized by remember { mutableStateOf(false) }
    val first = pages[0]
    val count = (first?.total ?: 0UL).coerceAtMost(Int.MAX_VALUE.toULong()).toInt()
    fun originalIndex(index: Int): Int = when {
        dragRow != null && dragIndex > dragOrigin && index in dragOrigin until dragIndex -> index + 1
        dragRow != null && dragIndex < dragOrigin && index in dragIndex + 1..dragOrigin -> index - 1
        else -> index
    }
    fun rowAt(index: Int): QueueRow? {
        if (dragRow != null && index == dragIndex) return dragRow
        val original = originalIndex(index)
        return pages[original / 80 * 80]?.rows?.getOrNull(original % 80)
    }
    fun updateDrag() {
        val middle = dragTop + dragDistance + dragHeight / 2f
        val target = list.layoutInfo.visibleItemsInfo.firstOrNull {
            it.index != dragIndex && middle >= it.offset && middle <= it.offset + it.size
        }
        val targetRow = target?.let { rowAt(it.index) }
        if (target != null && targetRow != null) {
            if (dragIndex == list.firstVisibleItemIndex || target.index == list.firstVisibleItemIndex)
                list.requestScrollToItem(list.firstVisibleItemIndex, list.firstVisibleItemScrollOffset)
            dragIndex = target.index
            haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick)
        }
    }
    fun close() = onActivated()
    fun openMenu(row: QueueRow) {
        player.model.browse.library?.trackItem(row.track())?.let { menu = row to BrowseItem(it, 0UL, null) }
    }
    LaunchedEffect(search) { delay(200); filter = search }
    LaunchedEffect(player, player.bridge, revision, output, filter, dragged == null) {
        if (dragged != null) return@LaunchedEffect
        try {
            val page = player.queuePage(0UL, filter) ?: return@LaunchedEffect
            val replacement = mutableMapOf(0 to page)
            for (chunk in list.layoutInfo.visibleItemsInfo.map { it.index / 80 * 80 }.distinct()) {
                if (chunk == 0 || chunk.toULong() >= page.total) continue
                val loaded = player.queuePage(page.windowOffset + chunk.toULong(), filter) ?: continue
                if (loaded.revision != page.revision) return@LaunchedEffect
                replacement[chunk] = loaded
            }
            pages = replacement
            dragRow = null
            // Two frames: the first lays the rows out under their new keys.
            if (reordered) launch { withFrameNanos { }; withFrameNanos { }; reordered = false }
            if (!initialized && filter.isEmpty()) {
                val index = player.playback?.queue?.currentIndex?.let { (it - minOf(it, page.windowOffset)).coerceAtMost(Int.MAX_VALUE.toULong()).toInt() } ?: 0
                list.scrollToItem(index.coerceIn(0, (page.total.toInt() - 1).coerceAtLeast(0)))
                initialized = true
            } else if (filter.isNotEmpty()) list.scrollToItem(0)
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
    }
    LaunchedEffect(first?.revision, filter, dragged == null) {
        snapshotFlow { list.layoutInfo.visibleItemsInfo.map { originalIndex(it.index) / 80 * 80 }.distinct() }.collectLatest { chunks ->
            val base = pages[0] ?: return@collectLatest
            try {
                for (chunk in chunks) if (chunk !in pages && chunk < count) {
                    val page = player.queuePage(base.windowOffset + chunk.toULong(), filter) ?: continue
                    if (page.revision != base.revision) return@collectLatest
                    pages = (pages + (chunk to page)).filterKeys { key -> key == 0 || chunks.any { kotlin.math.abs(key - it) <= 80 } }
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { player.reportError(error) }
        }
    }
    LaunchedEffect(dragged, dragScroll) {
        if (dragged == null || dragScroll == 0f) return@LaunchedEffect
        while (true) { list.scrollBy(dragScroll); updateDrag(); delay(16) }
    }
    fun startDrag(position: ULong, row: QueueRow) {
        val info = list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == position.toLong() } ?: return
        dragged = position; dragIndex = info.index; dragOrigin = info.index
        dragTop = info.offset.toFloat(); dragHeight = info.size
        dragDistance = 0f; dragRow = row; releasedIndex = null
        haptic.performHapticFeedback(HapticFeedbackType.LongPress)
    }
    fun dragBy(amount: Float) {
        dragDistance += amount
        updateDrag()
        val middle = dragTop + dragDistance + dragHeight / 2f
        val layout = list.layoutInfo
        dragScroll = when {
            middle < layout.viewportStartOffset + dragHeight -> -12f
            middle > layout.viewportEndOffset - dragHeight -> 12f
            else -> 0f
        }
    }
    fun cancelDrag() { dragged = null; dragRow = null; dragScroll = 0f }
    fun finishDrag() {
        val id = dragged
        val after = dragIndex > dragOrigin
        val target = rowAt(dragIndex + if (after) -1 else 1)?.position()
        val top = list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == id?.toLong() }?.offset ?: 0
        val offset = dragTop + dragDistance - top
        val moved = dragOrigin != dragIndex
        dragScroll = 0f
        if (id != null) scope.launch {
            val index = dragIndex
            releaseOffset.snapTo(offset)
            releasedIndex = index
            reordered = moved
            val settling = launch {
                releaseOffset.animateTo(0f, if (reduceMotion) tween(0)
                    else spring(stiffness = Spring.StiffnessMediumLow, visibilityThreshold = 1f))
            }
            try { if (moved) player.moveQueue(listOf(id), target, after) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { player.reportError(error) }
            finally { dragged = null }
            settling.join()
            if (releasedIndex == index) releasedIndex = null
        }
    }
    Column(modifier) {
        val total = player.playback?.queue?.total ?: 0UL
        if (selecting) Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 4.dp),
            verticalAlignment = Alignment.CenterVertically) {
            PlayerIconButton("rufin-window-close-symbolic", "Done", { selecting = false; picked = emptySet() })
            Text(translate("{count} selected").replace("{count}", picked.size.toString()), Modifier.weight(1f).padding(start = 4.dp),
                style = MaterialTheme.typography.titleMedium)
            val first = picked.minOrNull()
            val last = picked.maxOrNull()
            PlayerIconButton("rufin-go-up-symbolic", "Move up", {
                val from = first ?: return@PlayerIconButton
                val chosen = picked.sorted()
                scope.launch { player.moveQueue(chosen, from - 1UL) }
                picked = (0 until chosen.size).map { from - 1UL + it.toULong() }.toSet()
            }, enabled = first != null && first > 0UL)
            PlayerIconButton("rufin-go-down-symbolic", "Move down", {
                val to = last ?: return@PlayerIconButton
                val chosen = picked.sorted()
                scope.launch { player.moveQueue(chosen, to + 1UL, after = true) }
                val start = to + 2UL - chosen.size.toULong()
                picked = (0 until chosen.size).map { start + it.toULong() }.toSet()
            }, enabled = last != null && last + 1UL < total)
            PlayerIconButton("rufin-user-trash-symbolic", "Remove from Queue", {
                player.removeQueue(picked.toList()); picked = emptySet()
            }, enabled = picked.isNotEmpty())
        } else Row(Modifier.fillMaxWidth().padding(start = if (nowPlaying) 4.dp else 8.dp, end = if (nowPlaying) 4.dp else 8.dp, top = 4.dp),
            verticalAlignment = Alignment.CenterVertically) {
            val filled = (player.playback?.queue?.total ?: 0UL) > 0UL
            val tool = if (nowPlaying) Modifier else Modifier.size(40.dp)
            val searchTool: @Composable () -> Unit = {
                PlayerIconButton("rufin-search-symbolic", "Search", { searchVisible = !searchVisible; if (!searchVisible) search = "" },
                    tool, selected = searchVisible)
            }
            val createTool: @Composable () -> Unit = {
                PlayerIconButton("rufin-playlists-symbolic", "Create a playlist from queue", { create = true }, tool, enabled = filled)
            }
            val selectTool: @Composable () -> Unit = { PlayerIconButton("rufin-document-edit-symbolic", "Select", { selecting = true }, tool, enabled = filled) }
            val clearTool: @Composable () -> Unit = { PlayerIconButton("rufin-user-trash-symbolic", "Clear queue", { clear = true }, tool, enabled = filled) }
            // The sheet names the queue and counts it, then its tools. The pane beside a page is the desktop's:
            // one row of tools, no title, in the desktop's order; Select, which the desktop needs no button for, ends it.
            if (nowPlaying) {
                Column(Modifier.weight(1f).padding(start = 16.dp)) {
                    Text(translate("Queue"), style = MaterialTheme.typography.titleLarge)
                    Text(remember(player.playback?.queue?.total, translation) { player.playback?.queue?.total?.let(::trackCountText).orEmpty() },
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                searchTool(); createTool(); selectTool(); clearTool()
            } else {
                createTool(); clearTool(); searchTool()
                Spacer(Modifier.weight(1f))
                selectTool()
            }
        }
        if (nowPlaying) QueueNowPlaying(player)
        if (searchVisible) OutlinedTextField(search, { search = it }, singleLine = true,
            label = { Text(translate("Search")) }, modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 8.dp))
        LazyColumn(Modifier.fillMaxWidth().weight(1f), state = list,
            contentPadding = PaddingValues(top = 4.dp, bottom = 8.dp),
            verticalArrangement = Arrangement.spacedBy(2.dp)) {
            if (first == null) item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            else if (count == 0) item { Text(translate("Your queue is empty."), Modifier.padding(24.dp)) }
            items(count, key = { index -> rowAt(index)?.position()?.toLong() ?: "queue:$index" }) { index ->
                val row = rowAt(index)
                if (row == null) Spacer(Modifier.height(72.dp))
                else {
                    val position = remember(row) { row.position() }
                    val metadata = remember(row) { row.track().metadata }
                    val isReleased = releasedIndex == index
                    val isDragging = dragged == position && !isReleased
                    val placement = if (reduceMotion || isDragging || isReleased || reordered) Modifier
                        else Modifier.animateItem(fadeInSpec = null, fadeOutSpec = null)
                    val dismiss = rememberSwipeToDismissBoxState()
                    val shape = RoundedCornerShape(topStart = if (index == 0) 16.dp else 4.dp,
                        topEnd = if (index == 0) 16.dp else 4.dp, bottomStart = if (index == count - 1) 16.dp else 4.dp,
                        bottomEnd = if (index == count - 1) 16.dp else 4.dp)
                    Box(Modifier.padding(horizontal = 12.dp).then(placement)
                        .zIndex(if (isDragging || isReleased) 1f else 0f).graphicsLayer {
                            translationY = if (isDragging) {
                                val top = list.layoutInfo.visibleItemsInfo.firstOrNull { it.index == index }?.offset ?: 0
                                dragTop + dragDistance - top
                            } else if (isReleased) releaseOffset.value else 0f
                        }) {
                        SwipeToDismissBox(state = dismiss, gesturesEnabled = dragged == null,
                            onDismiss = {
                                haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                                scope.launch {
                                    dismiss.snapTo(SwipeToDismissBoxValue.Settled)
                                    player.removeQueue(position)
                                }
                            },
                            backgroundContent = {
                                Box(Modifier.fillMaxSize().clip(shape).background(MaterialTheme.colorScheme.errorContainer).padding(horizontal = 20.dp)) {
                                    RufinIcon("rufin-user-trash-symbolic", translate("Remove from Queue"), Modifier.size(24.dp)
                                        .align(if (dismiss.dismissDirection == SwipeToDismissBoxValue.StartToEnd) Alignment.CenterStart else Alignment.CenterEnd),
                                        MaterialTheme.colorScheme.onErrorContainer)
                                }
                            }) {
                            Surface(shape = shape, shadowElevation = if (isDragging) 8.dp else 0.dp) {
                                val current = position == currentIndex
                                ListItem(headlineContent = {
                                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                                        if (current && player.playback?.transport?.desiredPlaying == true) PlayingIndicator()
                                        Text(metadata.title, maxLines = 1, overflow = TextOverflow.Ellipsis,
                                            color = if (current && player.playback?.transport?.desiredPlaying == true) MaterialTheme.colorScheme.secondary else LocalContentColor.current)
                                    }
                                },
                                    supportingContent = { Text(listOf(metadata.artist, metadata.album).filter(String::isNotBlank).joinToString(" · "),
                                        maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                    leadingContent = { Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                                        if (selecting) io.github.screwys.rufin.browse.SelectionMark(position in picked)
                                        QueueSheetCover(row, player)
                                    } },
                                    // The handle drags at once; a hold anywhere on the row does too.
                                    trailingContent = { RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to reorder"), Modifier.size(20.dp)
                                        .pointerInput(row) {
                                            detectDragGestures(onDragStart = { startDrag(position, row) },
                                                onDrag = { change, amount -> change.consume(); dragBy(amount.y) },
                                                onDragEnd = { finishDrag() }, onDragCancel = { cancelDrag() })
                                        }, MaterialTheme.colorScheme.onSurfaceVariant) },
                                    colors = ListItemDefaults.colors(
                                        containerColor = if (position in picked) MaterialTheme.colorScheme.secondaryContainer
                                            else if (current) MaterialTheme.colorScheme.surfaceContainerHighest else MaterialTheme.colorScheme.surfaceContainerLow,
                                        headlineColor = if (current) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface),
                                    modifier = Modifier.semantics { contentDescription = translate("Hold to reorder") }
                                            .pointerInput(row) {
                                                detectDragGesturesAfterLongPress(onDragStart = { startDrag(position, row) },
                                                    onDrag = { change, amount -> change.consume(); dragBy(amount.y) }, onDragEnd = {
                                                    // A hold that did not move opens the track's menu.
                                                    if (dragOrigin == dragIndex && kotlin.math.abs(dragDistance) < 12f) {
                                                        cancelDrag(); openMenu(row)
                                                    } else finishDrag()
                                                }, onDragCancel = { cancelDrag() })
                                            }.clickable {
                                                if (selecting) picked = if (position in picked) picked - position else picked + position
                                                else { player.activateQueue(position); close() }
                                            })
                            }
                        }
                    }
                }
            }
        }
    }
    if (clear) {
        var includeCurrent by remember { mutableStateOf(false) }
        AlertDialog(onDismissRequest = { clear = false }, title = { Text(translate("Clear queue")) },
            text = { Row(verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Clearing queue also clears the current song"), Modifier.weight(1f)); Switch(includeCurrent, { includeCurrent = it })
            } }, confirmButton = { TextButton({ player.clearQueue(includeCurrent); clear = false }) { Text(translate("Clear")) } },
            dismissButton = { TextButton({ clear = false }) { Text(translate("Cancel")) } })
    }
    if (create) {
        PlaylistCreateSheet(player.model, { create = false }) { name, sourceId, public ->
            player.createQueuePlaylist(name, sourceId, public)
            create = false
        }
    }
    menu?.let { (row, item) ->
        BrowseMenuSheet(player.model, player.model.browse, item, player, { menu = null },
            { playlist = item; menu = null }, { metadataEditor = item; menu = null },
            onRemove = { player.removeQueue(row.position()); menu = null },
            onNavigate = { route, title, source -> menu = null; close(); onNavigate(route, title, source) },
            onPlayNow = { shuffled -> player.model.runAction {
                if (shuffled) withContext(Dispatchers.IO) { player.service?.setShuffle(true) }
                player.activateQueue(row.position()); close()
            } })
    }
    playlist?.let { item -> PlaylistPickerSheet(player.model, player.model.browse, item, player) { playlist = null } }
    metadataEditor?.row?.metadataItem?.let { item -> MetadataEditorScreen(player.model, item) { metadataEditor = null } }
}

@Composable
private fun QueueSheetCover(row: QueueRow, player: PlayerConnection) {
    val pixels = with(LocalDensity.current) { 48.dp.roundToPx() }
    val identity = remember(row) { row.track().artworkBinding }
    val artwork = rememberArtwork(identity, player, pixels)
    Box(Modifier.size(48.dp)) {
        Artwork(artwork, Modifier.fillMaxSize().clip(CoverShape))
    }
}

/** The playing track over the queue: its cover and names, play and pause, and a thin progress line. */
@Composable
private fun QueueNowPlaying(player: PlayerConnection) {
    val current = player.currentItem ?: return
    val playing by remember(player) { derivedStateOf { player.playback?.transport?.desiredPlaying == true } }
    val duration = player.playback?.transport?.durationMillis ?: 0UL
    val artwork = rememberArtwork(current.artworkBinding, player, with(LocalDensity.current) { 56.dp.roundToPx() })
    var position by remember { mutableStateOf(0UL) }
    val interpolate = playing && !LocalReduceMotion.current
    LaunchedEffect(player, interpolate) { do { position = player.positionNow(); if (interpolate) withFrameNanos { } } while (interpolate) }
    Surface(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp), shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHigh) {
        Column {
            Row(Modifier.padding(10.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Artwork(artwork, Modifier.size(56.dp).clip(CoverShape))
                Column(Modifier.weight(1f)) {
                    Text(current.metadata.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                    Text(current.metadata.artist, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                FilledIconButton(player::togglePlayback, Modifier.size(44.dp)) {
                    RufinIcon(if (playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                        translate(if (playing) "Pause" else "Play"), Modifier.size(22.dp))
                }
            }
            if (duration > 0UL) LinearProgressIndicator(progress = { (position.toDouble() / duration.toDouble()).toFloat().coerceIn(0f, 1f) },
                Modifier.fillMaxWidth().height(3.dp), drawStopIndicator = {})
        }
    }
}
