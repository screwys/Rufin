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
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
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
import io.github.screwys.rufin.core.AndroidQueuePage
import io.github.screwys.rufin.core.AndroidQueueRow
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.browse.BrowseItem
import io.github.screwys.rufin.browse.BrowseMenuSheet
import io.github.screwys.rufin.browse.PlaylistPickerSheet
import io.github.screwys.rufin.browse.PlaylistCreateSheet
import io.github.screwys.rufin.browse.CompactMediaMenuHeader
import io.github.screwys.rufin.ui.PlayingIndicator
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.browse.MediaMenuBackdrop
import io.github.screwys.rufin.metadata.MetadataEditorScreen
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.settings.PreferencesConnection
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
internal fun PlayerQueueSheet(player: PlayerConnection, preferences: PreferencesConnection, onDismiss: () -> Unit,
    onNavigate: (String, String, String?) -> Unit = { route, title, source -> player.model.browse.openLinkedRoute(route, title, source) },
) {
    val sheet = rememberModalBottomSheetState()
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    val reduceMotion = LocalReduceMotion.current
    val translation = LocalTranslationRevision.current
    val revision by remember(player) { derivedStateOf { player.playback?.queueRevision } }
    val output by remember(player) { derivedStateOf { player.playback?.let { it.outputKind to it.outputId } } }
    val currentOccurrence by remember(player) { derivedStateOf { player.playback?.occurrenceId } }
    var searchVisible by remember { mutableStateOf(false) }
    var search by remember { mutableStateOf("") }
    var filter by remember { mutableStateOf("") }
    var pages by remember { mutableStateOf<Map<Int, AndroidQueuePage>>(emptyMap()) }
    var selected by remember { mutableStateOf<AndroidQueueRow?>(null) }
    var menu by remember { mutableStateOf<Pair<AndroidQueueRow, BrowseItem>?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var metadataEditor by remember { mutableStateOf<BrowseItem?>(null) }
    var create by remember { mutableStateOf(false) }
    var clear by remember { mutableStateOf(false) }
    var dragged by remember { mutableStateOf<String?>(null) }
    var dragIndex by remember { mutableIntStateOf(0) }
    var dragOrigin by remember { mutableIntStateOf(0) }
    var dragTop by remember { mutableFloatStateOf(0f) }
    var dragHeight by remember { mutableIntStateOf(0) }
    var dragDistance by remember { mutableFloatStateOf(0f) }
    var dragRow by remember { mutableStateOf<AndroidQueueRow?>(null) }
    var released by remember { mutableStateOf<String?>(null) }
    val releaseOffset = remember { Animatable(0f) }
    var dragScroll by remember { mutableFloatStateOf(0f) }
    var initialized by remember { mutableStateOf(false) }
    val first = pages[0]
    val count = (first?.windowCount ?: 0UL).coerceAtMost(Int.MAX_VALUE.toULong()).toInt()
    fun originalIndex(index: Int): Int = when {
        dragRow != null && dragIndex > dragOrigin && index in dragOrigin until dragIndex -> index + 1
        dragRow != null && dragIndex < dragOrigin && index in dragIndex + 1..dragOrigin -> index - 1
        else -> index
    }
    fun rowAt(index: Int): AndroidQueueRow? {
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
    fun close() { scope.launch { sheet.hide(); onDismiss() } }
    fun openMenu(row: AndroidQueueRow) {
        scope.launch {
            try {
                val facts = player.model.browse.library?.trackItem(row.mediaUri)
                if (facts != null) menu = row to BrowseItem(facts, 0UL, null)
                else selected = row
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { player.reportError(error); selected = row }
        }
    }
    LaunchedEffect(search) { delay(200); filter = search }
    LaunchedEffect(player, player.bridge, revision, output, filter, dragged == null) {
        if (dragged != null) return@LaunchedEffect
        try {
            val page = player.queuePage(0UL, filter) ?: return@LaunchedEffect
            val replacement = mutableMapOf(0 to page)
            for (chunk in list.layoutInfo.visibleItemsInfo.map { it.index / 80 * 80 }.distinct()) {
                if (chunk == 0 || chunk.toULong() >= page.windowCount) continue
                val loaded = player.queuePage(page.windowOffset + chunk.toULong(), filter) ?: continue
                if (loaded.revision != page.revision) return@LaunchedEffect
                replacement[chunk] = loaded
            }
            pages = replacement
            dragRow = null
            if (!initialized && filter.isEmpty()) {
                val index = player.playback?.queueIndex?.let { (it - minOf(it, page.windowOffset)).coerceAtMost(Int.MAX_VALUE.toULong()).toInt() } ?: 0
                list.scrollToItem(index.coerceIn(0, (page.windowCount.toInt() - 1).coerceAtLeast(0)))
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
    fun finishDrag() {
        val id = dragged
        val after = dragIndex > dragOrigin
        val target = rowAt(dragIndex + if (after) -1 else 1)?.occurrenceId
        val top = list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == id }?.offset ?: 0
        val offset = dragTop + dragDistance - top
        val moved = dragOrigin != dragIndex
        dragScroll = 0f
        if (id != null) scope.launch {
            releaseOffset.snapTo(offset)
            released = id
            val settling = launch {
                releaseOffset.animateTo(0f, if (reduceMotion) tween(0)
                    else spring(stiffness = Spring.StiffnessMediumLow, visibilityThreshold = 1f))
            }
            try { if (moved) player.moveQueue(id, target, after) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { player.reportError(error) }
            finally { dragged = null }
            settling.join()
            if (released == id) released = null
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet,
        contentWindowInsets = { WindowInsets.systemBars.only(WindowInsetsSides.Top) },
        dragHandle = { Box(Modifier.padding(vertical = 5.dp).size(32.dp, 4.dp)
            .background(MaterialTheme.colorScheme.primary, RoundedCornerShape(4.dp))) }) {
        Column(Modifier.fillMaxHeight(.92f)) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Text(remember(first?.total, translation) { first?.total?.let(::trackCountText).orEmpty() },
                    Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                PlayerIconButton("rufin-search-symbolic", "Search", { searchVisible = !searchVisible; if (!searchVisible) search = "" })
                PlayerIconButton("rufin-playlists-symbolic", "New Playlist", { create = true }, enabled = (first?.total ?: 0UL) > 0UL)
                PlayerIconButton("rufin-user-trash-symbolic", "Clear queue", { clear = true }, enabled = (first?.total ?: 0UL) > 0UL)
            }
            if (searchVisible) OutlinedTextField(search, { search = it }, singleLine = true,
                label = { Text(translate("Search")) }, modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 8.dp))
            LazyColumn(Modifier.fillMaxWidth().weight(1f), state = list,
                contentPadding = WindowInsets.systemBars.only(WindowInsetsSides.Bottom).asPaddingValues(),
                verticalArrangement = Arrangement.spacedBy(2.dp)) {
                if (first == null) item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
                else if (count == 0) item { Text(translate("Your queue is empty."), Modifier.padding(24.dp)) }
                items(count, key = { index -> rowAt(index)?.occurrenceId ?: "queue:$index" }) { index ->
                    val row = rowAt(index)
                    if (row == null) Spacer(Modifier.height(72.dp))
                    else {
                        val isDragging = dragged == row.occurrenceId && released != row.occurrenceId
                        val isReleased = released == row.occurrenceId
                        val placement = if (reduceMotion || isDragging || isReleased) Modifier
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
                                onDismiss = { haptic.performHapticFeedback(HapticFeedbackType.LongPress); player.removeQueue(row.occurrenceId) },
                                backgroundContent = {
                                    Box(Modifier.fillMaxSize().clip(shape).background(MaterialTheme.colorScheme.errorContainer).padding(horizontal = 20.dp)) {
                                        RufinIcon("rufin-user-trash-symbolic", translate("Remove from Queue"), Modifier.size(24.dp)
                                            .align(if (dismiss.dismissDirection == SwipeToDismissBoxValue.StartToEnd) Alignment.CenterStart else Alignment.CenterEnd),
                                            MaterialTheme.colorScheme.onErrorContainer)
                                    }
                                }) {
                                Surface(shape = shape, shadowElevation = if (isDragging) 8.dp else 0.dp) {
                                    val current = row.occurrenceId == currentOccurrence
                                    ListItem(headlineContent = {
                                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                                            if (current && player.playback?.desiredPlaying == true) PlayingIndicator()
                                            Text(row.title, maxLines = 1, overflow = TextOverflow.Ellipsis,
                                                color = if (current && player.playback?.desiredPlaying == true) MaterialTheme.colorScheme.secondary else LocalContentColor.current)
                                        }
                                    },
                                        supportingContent = { Text(listOf(row.artist, row.album).filter(String::isNotBlank).joinToString(" · "),
                                            maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                        leadingContent = { QueueSheetCover(row, player) },
                                        trailingContent = {
                                            Box(Modifier.size(40.dp).semantics { contentDescription = translate("Reorder") }
                                                .pointerInput(row.occurrenceId) {
                                                    detectDragGestures(onDragStart = {
                                                        val info = list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == row.occurrenceId }
                                                        if (info != null) {
                                                            dragged = row.occurrenceId; dragIndex = info.index; dragOrigin = info.index
                                                            dragTop = info.offset.toFloat(); dragHeight = info.size
                                                            dragDistance = 0f; dragRow = row; released = null
                                                            haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                                                        }
                                                    }, onDrag = { change, amount ->
                                                        change.consume(); dragDistance += amount.y
                                                        updateDrag()
                                                        val middle = dragTop + dragDistance + dragHeight / 2f
                                                        val layout = list.layoutInfo
                                                        dragScroll = when {
                                                            middle < layout.viewportStartOffset + dragHeight -> -12f
                                                            middle > layout.viewportEndOffset - dragHeight -> 12f
                                                            else -> 0f
                                                        }
                                                    }, onDragEnd = ::finishDrag, onDragCancel = { dragged = null; dragRow = null; dragScroll = 0f })
                                                }, contentAlignment = Alignment.Center) {
                                                RufinIcon("rufin-list-drag-handle-symbolic", null, Modifier.size(20.dp))
                                            }
                                        }, colors = ListItemDefaults.colors(
                                            containerColor = if (current) MaterialTheme.colorScheme.surfaceContainerHighest else MaterialTheme.colorScheme.surfaceContainerLow,
                                            headlineColor = if (current) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface),
                                        modifier = Modifier.combinedClickable(onClick = {
                                            player.activateQueue(row.occurrenceId); close()
                                        }, onLongClick = { openMenu(row) }))
                                }
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
        val hasSource = player.model.sources?.selectedSourceId != null
        PlaylistCreateSheet(player.model, { create = false },
            initialCurrentSource = hasSource && preferences.applicationSettings?.optBoolean("new_playlist_current", true) != false,
        ) { name, sourceId, public ->
            player.createQueuePlaylist(name, sourceId != null, public)
            preferences.refreshApplicationSettings()
            create = false
        }
    }
    selected?.let { row ->
        ModalBottomSheet(onDismissRequest = { selected = null }, containerColor = androidx.compose.ui.graphics.Color.Transparent,
            dragHandle = null, sheetState = rememberModalBottomSheetState()) {
            val favorite = player.projectedFavorite(row.mediaUri, row.favorite)
            val toggleFavorite: () -> Unit = {
                scope.launch {
                    try {
                        val requested = !favorite
                        val result = player.queueFavorite(row.mediaUri, requested)
                        if (result != null && selected?.occurrenceId == row.occurrenceId &&
                            player.projectedFavorite(row.mediaUri, requested) == requested)
                            selected = selected?.copy(favorite = result)
                    }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { player.reportError(error) }
                }
            }
            MediaMenuBackdrop(row.artworkIdentity, player) {
                Column(Modifier.fillMaxWidth().windowInsetsPadding(WindowInsets.safeDrawing).padding(vertical = 8.dp)) {
                    BottomSheetDefaults.DragHandle(Modifier.align(Alignment.CenterHorizontally))
                    CompactMediaMenuHeader(row.artworkIdentity, listOf(row.artist, row.album).filter(String::isNotBlank).joinToString(" · "),
                        favorite, player, toggleFavorite) { Text(row.title, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1, style = MaterialTheme.typography.bodyLarge) }
                    QueueSheetAction("rufin-media-playback-start-symbolic", "Play") { player.activateQueue(row.occurrenceId); selected = null; close() }
                    QueueSheetAction("rufin-user-trash-symbolic", "Remove from Queue") { player.removeQueue(row.occurrenceId); selected = null }
                }
            }
        }
    }
    menu?.let { (row, item) ->
        BrowseMenuSheet(player.model, player.model.browse, item, player, { menu = null },
            { playlist = item; menu = null }, { metadataEditor = item; menu = null },
            onRemove = { player.removeQueue(row.occurrenceId); menu = null },
            onNavigate = { route, title, source -> menu = null; close(); onNavigate(route, title, source) },
            onPlayNow = { shuffled -> player.model.runAction {
                if (shuffled) withContext(Dispatchers.IO) { player.service?.setShuffle(true) }
                player.activateQueue(row.occurrenceId); close()
            } })
    }
    playlist?.let { item -> PlaylistPickerSheet(player.model, player.model.browse, item, player) { playlist = null } }
    metadataEditor?.let { item -> MetadataEditorScreen(player.model, item.row.kind, item.row.mediaUri) { metadataEditor = null } }
}

@Composable
private fun QueueSheetCover(row: AndroidQueueRow, player: PlayerConnection) {
    val pixels = with(LocalDensity.current) { 48.dp.roundToPx() }
    val artwork = rememberArtwork(row.artworkIdentity, player, pixels)
    Box(Modifier.size(48.dp)) {
        Artwork(artwork, Modifier.fillMaxSize().clip(RoundedCornerShape(10.dp)))
    }
}

@Composable
private fun QueueSheetAction(icon: String, label: String, action: () -> Unit) {
    ListItem(headlineContent = { Text(translate(label)) }, leadingContent = { RufinIcon(icon, null, Modifier.size(24.dp)) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier.clickable(onClick = action))
}
