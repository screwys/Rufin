package io.github.screwys.rufin.browse

import android.content.ClipData
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.draganddrop.dragAndDropSource
import androidx.compose.foundation.draganddrop.dragAndDropTarget
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draganddrop.DragAndDropEvent
import androidx.compose.ui.draganddrop.DragAndDropTarget
import androidx.compose.ui.draganddrop.DragAndDropTransferData
import androidx.compose.ui.draganddrop.toAndroidDragEvent
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidBrowseItem
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.library.PlaylistFolderMember
import io.github.screwys.rufin.shared.library.PlaylistFolderRow
import io.github.screwys.rufin.ui.RufinIcon

private data class PlaylistDrag(val member: PlaylistFolderMember, val smart: Boolean)

@Composable
internal fun Modifier.playlistFolderDrag(item: BrowseItem, model: RufinConnection): Modifier {
    val member = item.row.playlistFolderMember
    val folder = (item.row.item as? AndroidBrowseItem.PlaylistFolder)?.row
    if (member != null) return dragAndDropSource(transferData = { _ ->
        DragAndDropTransferData(ClipData.newPlainText("Rufin playlist", ""),
            localState = PlaylistDrag(member, member is PlaylistFolderMember.SmartPlaylist))
    })
    if (folder == null) return this
    var targeted by remember(folder.folderKey) { mutableStateOf(false) }
    val target = remember(folder.folderKey, model) {
        object : DragAndDropTarget {
            override fun onEntered(event: DragAndDropEvent) { targeted = true }
            override fun onExited(event: DragAndDropEvent) { targeted = false }
            override fun onEnded(event: DragAndDropEvent) { targeted = false }
            override fun onDrop(event: DragAndDropEvent): Boolean {
                targeted = false
                val drag = event.toAndroidDragEvent().localState as? PlaylistDrag ?: return false
                if (drag.smart != folder.smart) return false
                model.runAction {
                    model.browse.library?.movePlaylistToFolder(drag.member, folder.folderKey)
                    model.browse.refresh()
                }
                return true
            }
        }
    }
    return background(if (targeted) MaterialTheme.colorScheme.secondaryContainer else androidx.compose.ui.graphics.Color.Transparent)
        .dragAndDropTarget(shouldStartDragAndDrop = { event ->
            (event.toAndroidDragEvent().localState as? PlaylistDrag)?.smart == folder.smart
        }, target = target)
}

@Composable
internal fun PlaylistFolderNameDialog(
    model: RufinConnection, browse: BrowseConnection, smart: Boolean, onDismiss: () -> Unit,
    folder: PlaylistFolderRow? = null,
) {
    var name by remember(folder?.folderKey) { mutableStateOf(folder?.name.orEmpty()) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate(if (folder == null) "New folder" else "Rename folder")) },
        text = { OutlinedTextField(name, { name = it }, label = { Text(translate("Folder name")) }, singleLine = true) },
        confirmButton = { TextButton({ model.runAction {
            val library = browse.library ?: return@runAction
            if (folder == null) library.createPlaylistFolder(name.trim(), smart)
            else library.renamePlaylistFolder(folder.folderKey, name.trim())
            browse.refresh()
            onDismiss()
        } }, enabled = name.isNotBlank() && model.pendingActions == 0) {
            Text(translate(if (folder == null) "Create" else "Save"))
        } }, dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlaylistFolderPicker(model: RufinConnection, browse: BrowseConnection, item: BrowseItem, onDismiss: () -> Unit) {
    val member = item.row.playlistFolderMember ?: return
    val smart = member is PlaylistFolderMember.SmartPlaylist
    var folders by remember { mutableStateOf<List<PlaylistFolderRow>?>(null) }
    var create by remember { mutableStateOf(false) }
    var revision by remember { mutableIntStateOf(0) }
    LaunchedEffect(browse.library, smart, revision) {
        try { folders = browse.library?.playlistFolders(smart) }
        catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
        catch (failure: Exception) { model.runAction { throw failure } }
    }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(translate("Add to folder"), Modifier.padding(horizontal = 24.dp, vertical = 12.dp), style = MaterialTheme.typography.titleLarge)
        BrowseMenuAction("rufin-folder-new-symbolic", "New folder") { create = true }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            if (folders == null) item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
            items(folders.orEmpty(), key = { it.folderKey }) { folder ->
                ListItem(headlineContent = { Text(folder.name) }, leadingContent = { RufinIcon("rufin-folders-symbolic", null) },
                    modifier = Modifier.clickable { model.runAction {
                        browse.library?.movePlaylistToFolder(member, folder.folderKey)
                        browse.refresh()
                        onDismiss()
                    } })
            }
        }
    }
    if (create) PlaylistFolderNameDialog(model, browse, smart, { create = false; revision++ })
}
