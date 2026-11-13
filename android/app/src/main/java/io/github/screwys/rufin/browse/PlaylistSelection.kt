package io.github.screwys.rufin.browse

import androidx.compose.ui.semantics.selected

import androidx.compose.ui.semantics.semantics

import androidx.compose.foundation.verticalScroll

import androidx.compose.foundation.rememberScrollState

import io.github.screwys.rufin.settings.SettingsSwitch

import io.github.screwys.rufin.settings.SettingsRow

import io.github.screwys.rufin.settings.SettingsGroup

import io.github.screwys.rufin.shared.library.PlaylistFolderMember
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.rufin_core.addToPlaylistFolder
import io.github.screwys.rufin.shared.rufin_core.deletePlaylist
import io.github.screwys.rufin.shared.rufin_core.deletePlaylistFolder
import io.github.screwys.rufin.shared.rufin_core.removeFromPlaylistFolder
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.LocalPinnableContainer
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.util.lerp
import androidx.compose.ui.zIndex
import io.github.screwys.rufin.settings.LocalReduceMotion
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
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
    /** The picks once the row at `from` moved to `to`: picks follow their rows. */
    fun moved(from: ULong, to: ULong): Picked = if (all) this else copy(rows = rows.mapTo(mutableSetOf()) { index ->
        when {
            index == from -> to
            from < to && index in from + 1UL..to -> index - 1UL
            to < from && index in to until from -> index + 1UL
            else -> index
        }
    })
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
 *  Play next, Play later, Edit metadata and Delete permanently. */
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
    var deleting by remember { mutableStateOf(false) }
    val deletable = model.preferences?.player?.application?.allowPermanentDelete == true
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
    val list = rememberLazyListState()
    val carry = remember(list) { RowCarry(list) }
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    val reduceMotion = LocalReduceMotion.current
    // A dropped row's move is saved once it lands; the rows keep their places until the list shows
    // it, when another row is at the origin.
    var held by remember { mutableStateOf<Any?>(null) }
    val origin = carry.origin
    val shown = held != null && origin != null && rows.peek(origin)?.row?.key != held
    if (shown) SideEffect {
        held = null
        picked = picked.moved(origin.toULong(), (carry.over ?: origin).toULong())
        scope.launch { carry.reset() }
    }
    val carrying = origin != null && !shown
    fun shift(index: Int): Float {
        val from = origin ?: return 0f
        val to = carry.over ?: from
        return when {
            to > from && index in from + 1..to -> -carry.height.toFloat()
            to < from && index in to until from -> carry.height.toFloat()
            else -> 0f
        }
    }
    fun finish() {
        val from = carry.origin ?: return
        val to = carry.over ?: from
        scope.launch {
            carry.land(to, reduceMotion)
            val entry = rows.peek(from)?.row?.track?.entry
            val key = playlistRow?.playlistKey
            if (to == from || entry == null || key == null) return@launch carry.reset()
            held = rows.peek(from)?.row?.key
            model.runAction {
                try { library?.movePlaylistEntry(key, entry.key, to.toULong()) }
                catch (error: Exception) { held = null; carry.reset(); throw error }
            }
        }
    }
    CarryScroll(carry) { haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick) }
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
                    Box(Modifier.fillMaxWidth().weight(1f)) {
                        LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(bottom = 96.dp)) {
                            items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
                                val item = rows[index]
                                val carried = carrying && index == origin
                                PinWhile(carried)
                                // The rows the carried one passes make room for it.
                                val room by animateFloatAsState(if (carrying) shift(index) else 0f,
                                    if (reduceMotion) tween(0) else spring(stiffness = Spring.StiffnessMediumLow), label = "room")
                                if (item == null) Spacer(Modifier.height(60.dp))
                                else SelectRow(item, player, picked.has(item.index), reorder,
                                    Modifier.graphicsLayer { translationY = if (carrying) room else 0f; alpha = if (carried) 0f else 1f },
                                    { picked = picked.toggle(item.index, total) },
                                    Carrying(index to item.row.key, {
                                        (held == null && carry.start(index)).also { if (it) haptic.performHapticFeedback(HapticFeedbackType.LongPress) }
                                    }, { if (carry.by(it)) haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick) }, ::finish))
                            }
                        }
                        if (carrying) rows.peek(origin)?.let { item ->
                            CarriedCopy(carry) { SelectRow(item, player, picked.has(item.index), reorder, Modifier, {}, null) }
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
                    onEdit = { editing = true }, deletable = deletable, onDelete = { deleting = true })
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
        if (deleting) AlertDialog(onDismissRequest = { deleting = false },
            title = { Text(translate("Delete Permanently?")) },
            text = { Text(translate("The files are deleted where they are stored. This cannot be undone.")) },
            confirmButton = { TextButton({ deleting = false; act { selection -> library?.deleteSelection(selection) }; picked = Picked() },
                colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text(translate("Delete")) } },
            dismissButton = { TextButton({ deleting = false }) { Text(translate("Cancel")) } })
        if (adding) PlaylistPickerSheet(model, browse, playlist, player, selectionOf = { selection() }) { adding = false }
        if (editing) io.github.screwys.rufin.metadata.MetadataEditorScreen(model, null,
            selection = { selection() ?: MemberSelection.fromUris(emptyList()) }) { editing = false }
    }
}

/** The page whose Select mode is open: a playlist's, or a playlist list's. Its full-screen
 *  mode holds the page until it closes. */
internal class SelectMode { var route by mutableStateOf<Route?>(null) }

internal val LocalSelectMode = staticCompositionLocalOf { SelectMode() }

/** A playlist list's Select mode, as a playlist's: pick rows or all of them and act on them: Play,
 *  Add to…, Download, Add to folder (Remove from folder on a folder's page), and under More Play
 *  next, Play later and Delete. A playlist is carried by its handle or a hold, with the others
 *  picked: dropped on a folder they go into it, on another playlist they make a new folder with it. */
@OptIn(ExperimentalMaterial3Api::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun PlaylistListSelectMode(model: RufinConnection, browse: BrowseConnection, page: BrowsePage,
    rows: LazyPagingItems<BrowseItem>, player: PlayerConnection, onExit: () -> Unit,
) {
    var picked by remember { mutableStateOf(Picked()) }
    var adding by remember { mutableStateOf(false) }
    var filing by remember { mutableStateOf(false) }
    var grouping by remember { mutableStateOf<List<PlaylistFolderMember>?>(null) }
    var more by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    val total = rows.itemCount
    val count = picked.count(total)
    val library = browse.library
    val folder = (page.location.route as? Route.PlaylistFolder)?.key
    fun pickedItems() = (0 until total).mapNotNull { rows.peek(it) }.filter { picked.has(it.index) }
    fun members() = pickedItems().mapNotNull { it.row.playlistFolderMember }
    fun selection(): MemberSelection? = page.pageView?.rowsSelection(picked.ranges(total))
    fun act(action: suspend (MemberSelection) -> Unit) = model.runAction {
        selection()?.use { action(it) }
    }
    val members = members()
    val list = rememberLazyListState()
    val carry = remember(list) { RowCarry(list) }
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    val reduceMotion = LocalReduceMotion.current
    // The playlists the finger carries, and once they went into a folder that takes them off this
    // list, the ones still to leave it.
    var carried by remember { mutableStateOf(emptyList<PlaylistFolderMember>()) }
    var leaving by remember { mutableStateOf<List<PlaylistFolderMember>?>(null) }
    // On a folder's page they stay listed: they come back out of the folder they went into.
    val leaves = page.location.route !is Route.PlaylistFolder
    val gone = leaving?.let { going -> (0 until total).none { rows.peek(it)?.row?.playlistFolderMember in going } } == true
    if (gone) SideEffect { leaving = null; scope.launch { carry.reset() } }
    fun accepts(index: Int): Boolean {
        val row = rows.peek(index)?.row ?: return false
        return index != carry.origin && (row.playlistFolder != null || row.playlistFolderMember?.let { it !in carried } == true)
    }
    fun unfold() {
        val from = carry.origin ?: return
        scope.launch {
            coroutineScope {
                launch { carry.land(from, reduceMotion) }
                launch { carry.folding.animateTo(0f, tween(if (reduceMotion) 0 else 220)) }
            }
            carry.reset()
        }
    }
    fun finish() {
        val from = carry.origin ?: return
        val to = carry.over?.takeIf(::accepts)
        val target = to?.let { rows.peek(it)?.row }
        val dragged = carried
        scope.launch {
            if (to == null || target == null) {
                carry.land(from, reduceMotion)
                return@launch carry.reset()
            }
            // Into the folder or playlist it was dropped on.
            coroutineScope {
                launch { carry.land(to, reduceMotion) }
                launch { carry.folding.animateTo(1f, tween(if (reduceMotion) 0 else 260)) }
            }
            val folder = target.playlistFolder
            val with = target.playlistFolderMember
            if (folder != null) model.runAction {
                try {
                    addToPlaylistFolder(model.sourceOwner ?: error("No source"), dragged, folder.folderKey)
                    picked = Picked()
                    if (leaves) leaving = dragged else unfold()
                } catch (error: Exception) { unfold(); throw error }
            } else if (with != null) grouping = dragged + with
        }
    }
    CarryScroll(carry) { haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick) }
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
                    HorizontalDivider()
                    Box(Modifier.fillMaxWidth().weight(1f)) {
                        LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(bottom = 96.dp)) {
                            // Keyed by playlist, so the others close up when some go into a folder.
                            items(rows.itemCount, key = rows.itemKey { it.row.key }) { index ->
                                val item = rows[index]
                                val origin = carry.origin
                                PinWhile(index == origin)
                                // The folder or playlist under the carried one takes it.
                                val taking = origin != null && carry.over == index && accepts(index)
                                val grow by animateFloatAsState(if (taking) 1.04f else 1f, label = "taking")
                                // The other picked playlists go with it.
                                val going = origin != null && index != origin && item?.row?.playlistFolderMember in carried
                                if (item == null) Spacer(Modifier.height(64.dp))
                                else PlaylistPickRow(item, player, picked.has(item.index), taking,
                                    (if (reduceMotion) Modifier else Modifier.animateItem(fadeInSpec = null, fadeOutSpec = null))
                                        .zIndex(if (taking) 1f else 0f).graphicsLayer {
                                        scaleX = grow; scaleY = grow
                                        alpha = when {
                                            index == origin -> 0f
                                            going -> 1f - carry.folding.value
                                            else -> 1f
                                        }
                                    },
                                    { picked = picked.toggle(item.index, total) },
                                    item.row.playlistFolderMember?.let { member ->
                                        Carrying(index to item.row.key, {
                                            (leaving == null && grouping == null && carry.start(index)).also { started ->
                                                if (started) {
                                                    carried = if (picked.has(item.index)) members() else listOf(member)
                                                    haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                                                }
                                            }
                                        }, { if (carry.by(it)) haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick) }, ::finish)
                                    })
                            }
                        }
                        carry.origin?.let { origin -> rows.peek(origin) }?.let { item ->
                            CarriedCopy(carry) {
                                Box {
                                    PlaylistPickRow(item, player, picked.has(item.index), false, Modifier, {}, null)
                                    if (carried.size > 1) Surface(Modifier.align(Alignment.TopStart).padding(start = 64.dp, top = 4.dp),
                                        shape = CircleShape, color = MaterialTheme.colorScheme.primary) {
                                        Text(carried.size.toString(), Modifier.padding(horizontal = 7.dp, vertical = 1.dp),
                                            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onPrimary)
                                    }
                                }
                            }
                        }
                    }
                }
                Surface(Modifier.align(Alignment.BottomCenter).padding(horizontal = 12.dp, vertical = 14.dp).fillMaxWidth().height(64.dp),
                    shape = RoundedCornerShape(20.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh, shadowElevation = 8.dp) {
                    Row(Modifier.fillMaxSize().padding(horizontal = 4.dp)) {
                        val enabled = count > 0
                        SelectAction("rufin-media-playback-start-symbolic", "Play", enabled) {
                            act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.Replace(0UL), false) } }
                        }
                        SelectAction("rufin-list-add-symbolic", "Add to…", enabled) { adding = true }
                        SelectAction("rufin-download-symbolic", "Download", enabled) { act { selection -> library?.downloadSelection(selection, false) } }
                        // On a folder's page, as on a playlist's, Remove takes the picked ones out of it.
                        if (folder != null) SelectAction("rufin-list-remove-symbolic", "Remove", members.isNotEmpty()) {
                            model.runAction {
                                removeFromPlaylistFolder(model.sourceOwner ?: return@runAction, members, folder)
                                picked = Picked()
                            }
                        } else SelectAction("rufin-folders-symbolic", "Add to folder", members.isNotEmpty()) { filing = true }
                        Box(Modifier.weight(1f).fillMaxHeight()) {
                            SelectAction("rufin-view-more-symbolic", "More", enabled) { more = true }
                            DropdownMenu(more, { more = false }) {
                                DropdownMenuItem(text = { Text(translate("Play Next")) }, onClick = { more = false
                                    act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.AfterCurrent, false) } } },
                                    leadingIcon = { RufinIcon("rufin-mail-forward-symbolic", null, Modifier.size(20.dp)) })
                                DropdownMenuItem(text = { Text(translate("Play Later")) }, onClick = { more = false
                                    act { selection -> model.connectedService().playRequest { library?.playSelection(selection, QueuePlacement.End, false) } } },
                                    leadingIcon = { RufinIcon("rufin-go-last-symbolic", null, Modifier.size(20.dp)) })
                                if (pickedItems().any { library?.menu(it.row)?.delete == true }) {
                                    val danger = MaterialTheme.colorScheme.error
                                    DropdownMenuItem(text = { Text(translate("Delete"), color = danger) }, onClick = { more = false; deleting = true },
                                        leadingIcon = { RufinIcon("rufin-user-trash-symbolic", null, Modifier.size(20.dp), tint = danger) })
                                }
                            }
                        }
                    }
                }
            }
        }
        if (adding) PlaylistPickerSheet(model, browse, null, player, selectionOf = { selection() }) { adding = false }
        if (filing) PlaylistPickerSheet(model, browse, null, player, folderMembers = members) { filing = false; picked = Picked() }
        grouping?.let { dragged ->
            PlaylistFolderNameDialog(model, { grouping = null; if (leaving == null) unfold() }, members = dragged,
                onCreated = { picked = Picked(); if (leaves) leaving = dragged })
        }
        if (deleting) AlertDialog(onDismissRequest = { deleting = false },
            title = { Text(translate("Delete")) },
            text = { Text(translate("{count} selected").replace("{count}", count.toString())) },
            confirmButton = { TextButton({
                deleting = false
                val doomed = pickedItems().filter { library?.menu(it.row)?.delete == true }
                model.runAction {
                    val owner = model.sourceOwner ?: return@runAction
                    model.connectedService().runtime.value!!.getOrThrow().source().use { source ->
                        doomed.forEach { item ->
                            val row = item.row
                            when {
                                row.playlistFolder != null -> deletePlaylistFolder(owner, row.playlistFolder!!.folderKey)
                                row.smartPlaylist != null -> library?.deleteSmartPlaylist(row.smartPlaylist!!.smartPlaylistKey)
                                else -> (row.route as? Route.PlaylistDetail)?.let { deletePlaylist(source, it.v1) }
                            }
                        }
                    }
                    picked = Picked()
                }
            }, colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text(translate("Delete")) } },
            dismissButton = { TextButton({ deleting = false }) { Text(translate("Cancel")) } })
    }
}

/** A Select list row's drag: from its handle at once, or a hold anywhere on the row. `onStart`
 *  says whether the drag is this one's to carry. */
private class Carrying(val key: Any, val onStart: () -> Boolean, val onDrag: (Float) -> Unit, val onEnd: () -> Unit)

private fun Modifier.carries(carrying: Carrying?, hold: Boolean): Modifier = if (carrying == null) this else pointerInput(carrying.key) {
    // The detectors report a cancel whenever the row goes away, dragging or not: only a drag this
    // one started ends.
    var carried = false
    val start = { _: androidx.compose.ui.geometry.Offset -> carried = carrying.onStart() }
    val end = { if (carried) { carried = false; carrying.onEnd() } }
    val drag = { change: androidx.compose.ui.input.pointer.PointerInputChange, amount: androidx.compose.ui.geometry.Offset ->
        if (carried) { change.consume(); carrying.onDrag(amount.y) }
    }
    if (hold) detectDragGesturesAfterLongPress(start, end, end, drag)
    else detectDragGestures(start, end, end, drag)
}

/** A row a finger carries along a Select list, as the queue's: a copy follows the finger over the
 *  list, which scrolls near its edges, while the row keeps its place. The rows are of one height. */
private class RowCarry(val list: LazyListState) {
    var origin by mutableStateOf<Int?>(null)
    /** The row under the copy's middle. */
    var over by mutableStateOf<Int?>(null)
    var top by mutableFloatStateOf(0f)
    var height by mutableIntStateOf(0)
    var distance by mutableFloatStateOf(0f)
    var scroll by mutableFloatStateOf(0f)
    /** Released, the copy's top as it lands, and how far it went into a folder. */
    val landing = Animatable(0f)
    val folding = Animatable(0f)
    var released by mutableStateOf(false)

    fun start(index: Int): Boolean {
        if (origin != null) return false
        val info = list.layoutInfo.visibleItemsInfo.firstOrNull { it.index == index } ?: return false
        origin = index; over = index; top = info.offset.toFloat(); height = info.size; distance = 0f; released = false
        return true
    }

    /** Moves the copy by `amount`; true when it came over another row. */
    fun by(amount: Float): Boolean {
        if (origin == null || released) return false
        distance += amount
        val middle = top + distance + height / 2f
        val layout = list.layoutInfo
        scroll = when {
            middle < layout.viewportStartOffset + height -> -12f
            middle > layout.viewportEndOffset - height -> 12f
            else -> 0f
        }
        val under = layout.visibleItemsInfo.firstOrNull { middle >= it.offset && middle < it.offset + it.size }?.index
        if (under == null || under == over) return false
        over = under
        return true
    }

    /** Where row `index` is drawn now, from the list's top. */
    fun place(index: Int): Float {
        val first = list.layoutInfo.visibleItemsInfo.firstOrNull() ?: return 0f
        return first.offset + (index - first.index) * height.toFloat()
    }

    /** The copy's top now. */
    fun y(): Float = if (released) landing.value else top + distance

    /** Lets the copy go to row `index`'s place. */
    suspend fun land(index: Int, reduceMotion: Boolean) {
        landing.snapTo(y())
        released = true
        scroll = 0f
        landing.animateTo(place(index), if (reduceMotion) tween(0) else spring(stiffness = Spring.StiffnessMediumLow, visibilityThreshold = 1f))
    }

    suspend fun reset() {
        origin = null; over = null; released = false; scroll = 0f
        folding.snapTo(0f)
    }
}

/** Scrolls a carrying list while the copy is near an edge. */
@Composable
private fun CarryScroll(carry: RowCarry, onOver: () -> Unit) {
    LaunchedEffect(carry.origin, carry.scroll, carry.released) {
        if (carry.origin == null || carry.scroll == 0f || carry.released) return@LaunchedEffect
        while (true) {
            carry.list.scrollBy(carry.scroll)
            if (carry.by(0f)) onOver()
            delay(16)
        }
    }
}

/** Keeps a carried row composed while the list scrolls it away, so its drag goes on. */
@Composable
private fun PinWhile(carried: Boolean) {
    val pinnable = LocalPinnableContainer.current
    DisposableEffect(carried, pinnable) {
        val handle = if (carried) pinnable?.pin() else null
        onDispose { handle?.release() }
    }
}

/** The carried row's copy, lifted over the list; going into a folder, it shrinks and fades. */
@Composable
private fun BoxScope.CarriedCopy(carry: RowCarry, content: @Composable () -> Unit) {
    Surface(Modifier.align(Alignment.TopStart).fillMaxWidth().zIndex(2f).graphicsLayer {
        translationY = carry.y()
        val fold = carry.folding.value
        val scale = lerp(1.02f, .35f, fold)
        scaleX = scale; scaleY = scale
        alpha = 1f - fold
    }, shadowElevation = 8.dp, color = MaterialTheme.colorScheme.surfaceContainerHigh) { content() }
}

/** A playlist list row in Select: its mark, cover and title, and for a playlist the handle it is
 *  carried by. A tap picks it; `taking`, it shows that a drop goes into it. */
@Composable
private fun PlaylistPickRow(item: BrowseItem, player: PlayerConnection, picked: Boolean, taking: Boolean, modifier: Modifier,
    onToggle: () -> Unit, carrying: Carrying?,
) {
    val artwork = rememberArtwork(item.row.artworkIdentity, player, with(LocalDensity.current) { 44.dp.roundToPx() })
    val accent = MaterialTheme.colorScheme.primary
    Row(modifier.fillMaxWidth().height(64.dp)
        .background(when {
            taking -> MaterialTheme.colorScheme.secondaryContainer
            picked -> accent.copy(alpha = .1f)
            else -> androidx.compose.ui.graphics.Color.Transparent
        })
        .clickable(onClick = onToggle).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically) {
        // A hold carries the row from anywhere but its handle, which drags at once.
        Row(Modifier.weight(1f).fillMaxHeight().carries(carrying, hold = true), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            SelectionMark(picked)
            Artwork(artwork, Modifier.size(44.dp).clip(CoverShape))
            Column(Modifier.weight(1f)) {
                BrowseTitle(item, player = player, style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.SemiBold))
                if (item.row.subtitle.isNotBlank()) Text(item.row.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (item.row.playlistFolderMember != null) Box(Modifier.size(40.dp).carries(carrying, hold = false), contentAlignment = Alignment.Center) {
            RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to a folder"), Modifier.size(20.dp),
                MaterialTheme.colorScheme.onSurface.copy(alpha = .55f))
        }
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

/** A track row in a playlist's Select: its mark, cover and names, and in Playlist order the
 *  handle it is carried by to another place. */
@Composable
private fun SelectRow(item: BrowseItem, player: PlayerConnection, picked: Boolean, reorder: Boolean, modifier: Modifier,
    onToggle: () -> Unit, carrying: Carrying?,
) {
    val artwork = rememberArtwork(item.row.artworkIdentity, player, with(LocalDensity.current) { 44.dp.roundToPx() })
    val accent = MaterialTheme.colorScheme.primary
    val carried = carrying?.takeIf { reorder }
    Row(modifier.fillMaxWidth().height(60.dp)
        .background(if (picked) accent.copy(alpha = .1f) else androidx.compose.ui.graphics.Color.Transparent)
        .clickable(onClick = onToggle).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically) {
        // A hold carries the row from anywhere but its handle, which drags at once.
        Row(Modifier.weight(1f).fillMaxHeight().carries(carried, hold = true), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            SelectionMark(picked)
            Artwork(artwork, Modifier.size(44.dp).clip(CoverShape))
            Column(Modifier.weight(1f)) {
                Text(item.row.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge,
                    fontWeight = FontWeight.SemiBold)
                Text(listOf(item.row.artist, item.row.durationText).filter { it.isNotBlank() }.joinToString(" · "), maxLines = 1,
                    overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (reorder) Box(Modifier.size(32.dp).carries(carried, hold = false), contentAlignment = Alignment.Center) {
            RufinIcon("rufin-list-drag-handle-symbolic", translate("Drag to reorder"), Modifier.size(18.dp),
                MaterialTheme.colorScheme.onSurface.copy(alpha = .55f))
        }
    }
}

@Composable
private fun SelectActions(modifier: Modifier, enabled: Boolean, writable: Boolean,
    onPlay: () -> Unit, onAdd: () -> Unit, onDownload: () -> Unit, onRemove: () -> Unit,
    more: Boolean, onMore: (Boolean) -> Unit, onNext: () -> Unit, onLater: () -> Unit, onEdit: () -> Unit,
    deletable: Boolean, onDelete: () -> Unit,
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
                    if (deletable) {
                        val danger = MaterialTheme.colorScheme.error
                        DropdownMenuItem(text = { Text(translate("Delete permanently"), color = danger) }, onClick = { onMore(false); onDelete() },
                            leadingIcon = { RufinIcon("rufin-user-trash-symbolic", null, Modifier.size(20.dp), tint = danger) })
                    }
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
