package io.github.screwys.rufin.browse

import io.github.screwys.rufin.R
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.app.RufinConnection

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectDragGestures
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.ui.draw.clip
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.paging.LoadState
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.library.SidebarPin

/** The Saved page. Select (the bar's edit button) picks pins to move up or down together or to
 *  remove, and reorders one by its handle, as the queue's Select does. */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PinsContent(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection, page: BrowsePage,
    selecting: Boolean, onSelectEnd: () -> Unit,
) {
    SideEffect { browse.visiblePage = page }
    val rows = page.pages.collectAsLazyPagingItems()
    var action by remember { mutableStateOf<BrowseItem?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    var picked by remember { mutableStateOf(setOf<SidebarPin>()) }
    LaunchedEffect(selecting) { if (!selecting) picked = emptySet() }
    if (selecting) BackHandler(onBack = onSelectEnd)
    if (rows.itemSnapshotList.items.isEmpty() && (page.loading || rows.loadState.refresh is LoadState.Loading)) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val pins = rows.itemSnapshotList.items.mapNotNull { it.row.pin }
    Column(Modifier.fillMaxSize()) {
        if (selecting) Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            PlayerIconButton("rufin-window-close-symbolic", "Done", onSelectEnd)
            Text(translate("{count} selected").replace("{count}", picked.size.toString()), Modifier.weight(1f).padding(start = 4.dp),
                style = MaterialTheme.typography.titleMedium)
            val chosen = pins.filter { it in picked }
            val first = pins.indexOfFirst { it in picked }
            val last = pins.indexOfLast { it in picked }
            PlayerIconButton("rufin-go-up-symbolic", "Move up", { browse.movePins(chosen, pins[first - 1]) }, enabled = first > 0)
            PlayerIconButton("rufin-go-down-symbolic", "Move down", { browse.movePins(chosen.reversed(), pins[last + 1]) },
                enabled = last in 0 until pins.lastIndex)
            PlayerIconButton("rufin-user-trash-symbolic", "Remove from Saved", { browse.unpin(picked); picked = emptySet() },
                enabled = picked.isNotEmpty())
        }
        LazyColumn(Modifier.fillMaxWidth().weight(1f), contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
            items(rows.itemCount, key = rows.itemKey { it.row.pin.toString() }) { index ->
                val item = rows[index]
                val pin = item?.row?.pin
                if (item == null) Spacer(Modifier.height(64.dp))
                else if (selecting && pin != null) PinSelectRow(item, player, pin in picked,
                    { picked = if (pin in picked) picked - pin else picked + pin }) { rowsMoved ->
                    val target = (index + rowsMoved).coerceIn(0, pins.lastIndex)
                    if (target != index) browse.movePins(listOf(pin), pins[target])
                }
                else BrowseSwipeActions(item, true) {
                    ListItem(headlineContent = { BrowseTitle(page.shown(item), player = player) },
                        supportingContent = { PinFacts(item, model) },
                        leadingContent = {
                            val artwork = rememberArtwork(item.row.artworkIdentity, player, with(LocalDensity.current) { 50.dp.roundToPx() })
                            Artwork(artwork, Modifier.size(50.dp).clip(CoverShape))
                        },
                        trailingContent = { PlayerIconButton("rufin-view-more-symbolic", "More", { action = item }) },
                        modifier = Modifier.combinedClickable(onClick = { browse.open(item) }, onLongClick = { action = item }))
                }
            }
            if (!page.loading && rows.itemCount == 0 && rows.loadState.refresh is LoadState.NotLoading) item {
                BrowseEmptyState()
            }
            if (page.loading || rows.loadState.refresh is LoadState.Loading || rows.loadState.append is LoadState.Loading) item {
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            val failure = (rows.loadState.refresh as? LoadState.Error)?.error ?: (rows.loadState.append as? LoadState.Error)?.error
            failure?.let { error -> item { ErrorRow(error, translate("Retry"), rows::retry) } }
        }
    }
    action?.let { item -> BrowseMenuSheet(model, browse, item, player, { action = null },
        { playlist = item; action = null }, { metadata = item; action = null },
        onNavigate = { route, title, _ -> action = null; browse.openRoute(route, title) }) }
    playlist?.let { item -> PlaylistPickerSheet(model, browse, item, player) { playlist = null } }
    metadata?.row?.metadataItem?.let { item -> io.github.screwys.rufin.metadata.MetadataEditorScreen(model, item) { metadata = null } }
}

/** A pin's facts, as GTK's sidebar shows them: its tracks and length, and where a playlist lives
 *  (its source, or Rufin for a smart playlist). */
@Composable
internal fun PinFacts(item: BrowseItem, model: RufinConnection) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        SummaryFact("rufin-tracks-symbolic", "Tracks", item.row.trackCount.toString())
        SummaryFact("rufin-preferences-system-time-symbolic", "Duration", item.row.durationText)
        val pin = item.row.pin
        val source = (pin as? SidebarPin.Playlist)?.sourceId?.let { id -> model.sources?.sources?.find { it.id == id } }
        if (source != null) SourceIcon(source.kind, Modifier.size(14.dp))
        else if (pin is SidebarPin.Playlist || pin is SidebarPin.SmartPlaylist) Image(painterResource(R.drawable.app_icon), "Rufin", Modifier.size(14.dp))
    }
}

/** A pin in Select: its mark, cover, title and facts, and the handle it is dragged by. */
@Composable
private fun PinSelectRow(item: BrowseItem, player: PlayerConnection, picked: Boolean, onToggle: () -> Unit, onMoved: (Int) -> Unit) {
    val density = LocalDensity.current
    val artwork = rememberArtwork(item.row.artworkIdentity, player, with(density) { 50.dp.roundToPx() })
    var drag by remember { mutableFloatStateOf(0f) }
    val rowHeight = with(density) { 72.dp.toPx() }
    Row(Modifier.fillMaxWidth().height(72.dp).graphicsLayer { translationY = drag }
        .background(if (picked) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent)
        .clickable(onClick = onToggle).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
        SelectionMark(picked)
        Artwork(artwork, Modifier.size(50.dp).clip(CoverShape))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            BrowseTitle(item, player = player)
            PinFacts(item, player.model)
        }
        Box(Modifier.size(40.dp).pointerInput(item.row.pin) {
            detectDragGestures(onDragEnd = { onMoved(Math.round(drag / rowHeight)); drag = 0f }, onDragCancel = { drag = 0f }) { change, amount ->
                change.consume(); drag += amount.y
            }
        }, contentAlignment = Alignment.Center) {
            RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to reorder"), Modifier.size(20.dp),
                MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
