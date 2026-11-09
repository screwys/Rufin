package io.github.screwys.rufin.browse

import androidx.compose.ui.semantics.selected

import androidx.compose.ui.semantics.semantics

import androidx.compose.foundation.verticalScroll

import androidx.compose.foundation.rememberScrollState

import io.github.screwys.rufin.settings.SettingsSwitch

import io.github.screwys.rufin.settings.SettingsRow

import io.github.screwys.rufin.settings.SettingsGroup

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.paging.compose.LazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidRowRange
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.shared.library.MemberSelection
import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.CoverShape
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.rememberArtwork

/** Which rows a Select mode picked: some positions, or every row. */
private data class Picked(val all: Boolean = false, val rows: Set<ULong> = emptySet()) {
    fun count(total: Int) = if (all) total else rows.size
    fun has(index: ULong) = all || index in rows
    fun toggle(index: ULong, total: Int): Picked = when {
        all -> Picked(rows = (0UL until total.toULong()).toSet() - index)
        index in rows -> copy(rows = rows - index)
        else -> copy(rows = rows + index)
    }
    /** The picked positions as runs, as the page reads a selection. */
    fun ranges(total: Int): List<AndroidRowRange> {
        if (all) return listOf(AndroidRowRange(0UL, total.toULong()))
        val runs = mutableListOf<AndroidRowRange>()
        rows.sorted().forEach { index ->
            val last = runs.lastOrNull()
            if (last != null && last.end == index) runs[runs.lastIndex] = AndroidRowRange(last.start, index + 1UL)
            else runs += AndroidRowRange(index, index + 1UL)
        }
        return runs
    }
}

/** A playlist's Select mode: sort from its chip, pick rows or all of them, reorder by dragging
 *  in Playlist order, and act on what is picked: Play, Add to…, Download, Remove, and under More
 *  Play next, Play later and Edit metadata. */
@OptIn(ExperimentalMaterial3Api::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun PlaylistSelectMode(model: RufinConnection, browse: BrowseConnection, page: BrowsePage,
    rows: LazyPagingItems<BrowseItem>, player: PlayerConnection, playlist: BrowseItem, onExit: () -> Unit,
) {
    var picked by remember { mutableStateOf(Picked()) }
    var sorting by remember { mutableStateOf(false) }
    var adding by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf(false) }
    var more by remember { mutableStateOf(false) }
    val total = rows.itemCount
    val count = picked.count(total)
    val library = browse.library
    val playlistRow = playlist.row.playlist
    val writable = playlistRow?.writable == true
    // Playlist order is the order without a sort; only there do the rows move by dragging.
    val playlistOrder = page.options.sort == null
    val reorder = writable && playlistOrder && page.options.filter.isBlank()
    fun selection(): MemberSelection? = page.pageView?.rowsSelection(picked.ranges(total))
    fun act(action: suspend (MemberSelection) -> Unit) = model.runAction {
        selection()?.use { action(it) }
    }
    BackHandler(onBack = onExit)
    Dialog(onExit, DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Box(Modifier.fillMaxSize().systemBarsPadding()) {
                Column(Modifier.fillMaxSize()) {
                    Row(Modifier.fillMaxWidth().height(56.dp).padding(start = 4.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        PlayerIconButton("rufin-window-close-symbolic", "Leave Select mode", onExit)
                        Text(translate("{count} selected").replace("{count}", count.toString()), Modifier.weight(1f).padding(start = 4.dp),
                            style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                        TextButton({ picked = if (picked.all || count == total) Picked() else Picked(all = true) }) {
                            Text(translate(if (picked.all || (count == total && total > 0)) "Deselect all" else "Select all"))
                        }
                    }
                    Row(Modifier.fillMaxWidth().height(48.dp).padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        val sortTitle = page.sortFields.firstOrNull { it.id == page.options.sort }?.title ?: translate("Playlist order")
                        Row(Modifier.height(32.dp).clip(RoundedCornerShape(16.dp))
                            .background(MaterialTheme.colorScheme.surfaceContainerHighest)
                            .clickable { sorting = true }.padding(start = 10.dp, end = 12.dp),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                            RufinIcon(if (page.options.descending) "rufin-sort-name-descending-symbolic" else "rufin-sort-name-symbolic", null, Modifier.size(16.dp))
                            Text(sortTitle, style = MaterialTheme.typography.labelLarge)
                            RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(12.dp))
                        }
                        Spacer(Modifier.weight(1f))
                        if (writable) Text(translate(if (reorder) "Drag to reorder" else "Reorder in Playlist order"),
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    HorizontalDivider()
                    LazyColumn(Modifier.fillMaxWidth().weight(1f), contentPadding = PaddingValues(bottom = 96.dp)) {
                        items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
                            val item = rows[index]
                            if (item == null) Spacer(Modifier.height(60.dp))
                            else SelectRow(item, player, picked.has(item.index), reorder, { picked = picked.toggle(item.index, total) }) { rowsMoved ->
                                val entry = item.row.track?.entry ?: return@SelectRow
                                val key = playlistRow?.playlistKey ?: return@SelectRow
                                val target = (index + rowsMoved).coerceIn(0, total - 1).toULong()
                                if (target != index.toULong()) model.runAction { library?.movePlaylistEntry(key, entry.key, target) }
                            }
                        }
                    }
                }
                SelectActions(Modifier.align(Alignment.BottomCenter), count > 0, writable,
                    onPlay = { act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.Replace(0UL), false) } } },
                    onAdd = { adding = true },
                    onDownload = { act { selection -> library?.downloadSelection(selection, false) } },
                    onRemove = { act { selection -> playlistRow?.let { library?.removePlaylistEntries(it.playlistKey, selection) } }; picked = Picked() },
                    more = more, onMore = { more = it },
                    onNext = { act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.AfterCurrent, false) } } },
                    onLater = { act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.End, false) } } },
                    onEdit = { editing = true })
            }
        }
        // As the page's own sort sheet: the fields in one box, the picked one highlighted, and a switch.
        if (sorting) ModalBottomSheet({ sorting = false }) {
            Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp),
                verticalArrangement = Arrangement.spacedBy(20.dp)) {
                SettingsGroup(translate("Sort by")) {
                    SortOption(translate("Playlist order"), page.options.sort == null) { page.options.sort = null; page.options.descending = false; sorting = false }
                    // Playlist order is the position sort; "#" would offer it twice.
                    page.sortFields.filter { it.id != io.github.screwys.rufin.shared.library.LibraryField.ROW_INDEX }.forEach { field ->
                        SortOption(field.title, page.options.sort == field.id) { page.options.sort = field.id; sorting = false }
                    }
                }
                SettingsGroup { SettingsSwitch(translate("Reverse order"), page.options.descending) { page.options.descending = it } }
            }
        }
        if (adding) PlaylistPickerSheet(model, browse, playlist, player, selectionOf = { selection() }) { adding = false }
        if (editing) io.github.screwys.rufin.metadata.MetadataEditorScreen(model, null,
            selection = { selection() ?: MemberSelection.fromUris(emptyList()) }) { editing = false }
    }
}

@Composable
private fun SortOption(title: String, selected: Boolean, onClick: () -> Unit) {
    SettingsRow(title, selected = selected, modifier = Modifier.semantics { this.selected = selected }.clickable(onClick = onClick))
}

/** A Select mode's round mark in front of a row's artwork. */
@Composable
internal fun SelectionMark(picked: Boolean) {
    Box(Modifier.size(24.dp).clip(CircleShape).then(
        if (picked) Modifier.background(MaterialTheme.colorScheme.primary)
        else Modifier.border(2.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = .45f), CircleShape)),
        contentAlignment = Alignment.Center) {
        if (picked) RufinIcon("rufin-object-select-symbolic", translate("Selected"), Modifier.size(14.dp), MaterialTheme.colorScheme.onPrimary)
    }
}

@Composable
private fun SelectRow(item: BrowseItem, player: PlayerConnection, picked: Boolean, reorder: Boolean,
    onToggle: () -> Unit, onMoved: (Int) -> Unit,
) {
    val density = LocalDensity.current
    val artwork = rememberArtwork(item.row.artworkIdentity, player, with(density) { 44.dp.roundToPx() })
    var drag by remember { mutableFloatStateOf(0f) }
    val rowHeight = with(density) { 60.dp.toPx() }
    val accent = MaterialTheme.colorScheme.primary
    Row(Modifier.fillMaxWidth().height(60.dp).graphicsLayer { translationY = drag }
        .background(if (picked) accent.copy(alpha = .1f) else androidx.compose.ui.graphics.Color.Transparent)
        .clickable(onClick = onToggle).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
        SelectionMark(picked)
        Artwork(artwork, Modifier.size(44.dp).clip(CoverShape))
        Column(Modifier.weight(1f)) {
            Text(item.row.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold)
            Text(listOf(item.row.artist, item.row.durationText).filter { it.isNotBlank() }.joinToString(" · "), maxLines = 1,
                overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (reorder) Box(Modifier.size(32.dp).pointerInput(item.row.key) {
            detectDragGestures(onDragEnd = { onMoved(Math.round(drag / rowHeight)); drag = 0f }, onDragCancel = { drag = 0f }) { change, amount ->
                change.consume(); drag += amount.y
            }
        }, contentAlignment = Alignment.Center) {
            RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to reorder"), Modifier.size(18.dp),
                MaterialTheme.colorScheme.onSurface.copy(alpha = .55f))
        }
    }
}

@Composable
private fun SelectActions(modifier: Modifier, enabled: Boolean, writable: Boolean,
    onPlay: () -> Unit, onAdd: () -> Unit, onDownload: () -> Unit, onRemove: () -> Unit,
    more: Boolean, onMore: (Boolean) -> Unit, onNext: () -> Unit, onLater: () -> Unit, onEdit: () -> Unit,
) {
    Surface(modifier.padding(horizontal = 12.dp, vertical = 14.dp).fillMaxWidth().height(64.dp), shape = RoundedCornerShape(20.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHigh, shadowElevation = 8.dp) {
        Row(Modifier.fillMaxSize().padding(horizontal = 4.dp)) {
            SelectAction("rufin-media-playback-start-symbolic", "Play", enabled, onPlay)
            SelectAction("rufin-list-add-symbolic", "Add to…", enabled, onAdd)
            SelectAction("rufin-download-symbolic", "Download", enabled, onDownload)
            if (writable) SelectAction("rufin-list-remove-symbolic", "Remove", enabled, onRemove)
            Box(Modifier.weight(1f).fillMaxHeight()) {
                SelectAction("rufin-view-more-symbolic", "More", enabled) { onMore(true) }
                DropdownMenu(more, { onMore(false) }) {
                    DropdownMenuItem(text = { Text(translate("Play Next")) }, onClick = { onMore(false); onNext() },
                        leadingIcon = { RufinIcon("rufin-mail-forward-symbolic", null, Modifier.size(20.dp)) })
                    DropdownMenuItem(text = { Text(translate("Play Later")) }, onClick = { onMore(false); onLater() },
                        leadingIcon = { RufinIcon("rufin-go-last-symbolic", null, Modifier.size(20.dp)) })
                    DropdownMenuItem(text = { Text(translate("Edit metadata")) }, onClick = { onMore(false); onEdit() },
                        leadingIcon = { RufinIcon("rufin-document-edit-symbolic", null, Modifier.size(20.dp)) })
                }
            }
        }
    }
}

@Composable
private fun RowScope.SelectAction(icon: String, label: String, enabled: Boolean, onClick: () -> Unit) {
    Column(Modifier.weight(1f).fillMaxHeight().clickable(enabled = enabled, onClick = onClick).graphicsLayer { alpha = if (enabled) 1f else .4f },
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterVertically)) {
        RufinIcon(icon, null, Modifier.size(20.dp))
        Text(translate(label), style = MaterialTheme.typography.labelSmall, fontWeight = FontWeight.SemiBold, maxLines = 1)
    }
}

@Composable
private fun BoxScope.SelectAction(icon: String, label: String, enabled: Boolean, onClick: () -> Unit) {
    Column(Modifier.fillMaxSize().clickable(enabled = enabled, onClick = onClick).graphicsLayer { alpha = if (enabled) 1f else .4f },
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterVertically)) {
        RufinIcon(icon, null, Modifier.size(20.dp))
        Text(translate(label), style = MaterialTheme.typography.labelSmall, fontWeight = FontWeight.SemiBold, maxLines = 1)
    }
}
