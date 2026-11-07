package io.github.screwys.rufin.browse

import android.content.ClipData
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.draganddrop.DragAndDropSourceScope
import androidx.compose.foundation.draganddrop.dragAndDropSource
import androidx.compose.foundation.draganddrop.dragAndDropTarget
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draganddrop.DragAndDropEvent
import androidx.compose.ui.draganddrop.DragAndDropTarget
import androidx.compose.ui.draganddrop.DragAndDropTransferData
import androidx.compose.ui.draganddrop.toAndroidDragEvent
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.library.PlaylistFolderMember
import io.github.screwys.rufin.shared.library.PlaylistFolderRow
import io.github.screwys.rufin.shared.rufin_core.createPlaylistFolder
import io.github.screwys.rufin.shared.rufin_core.renamePlaylistFolder
import io.github.screwys.rufin.shared.rufin_core.movePlaylistToFolder
import io.github.screwys.rufin.ui.RufinIcon

private data class PlaylistDrag(val member: PlaylistFolderMember, val smart: Boolean)

/** The handle a playlist or smart playlist in a folder list is dragged by into a folder. */
@Suppress("DEPRECATION") // The newer source starts a touch drag on a long press, which opens the menu.
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun PlaylistDragHandle(member: PlaylistFolderMember, modifier: Modifier = Modifier) {
    val drag: suspend DragAndDropSourceScope.() -> Unit = {
        detectDragGestures(onDragStart = {
            startTransfer(DragAndDropTransferData(ClipData.newPlainText("Rufin playlist", ""),
                localState = PlaylistDrag(member, member is PlaylistFolderMember.SmartPlaylist)))
        }) { change, _ -> change.consume() }
    }
    Box(modifier.size(40.dp).dragAndDropSource(drag), contentAlignment = Alignment.Center) {
        RufinIcon("rufin-list-drag-handle-symbolic", null, Modifier.size(20.dp))
    }
}

/** A playlist folder takes the playlists dropped on it. */
@Composable
internal fun Modifier.playlistFolderDrop(item: BrowseItem, model: RufinConnection): Modifier {
    val folder = item.row.playlistFolder ?: return this
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
                    val source = model.sourceOwner ?: return@runAction
                    movePlaylistToFolder(source, drag.member, folder.folderKey)
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
    model: RufinConnection, smart: Boolean, onDismiss: () -> Unit,
    folder: PlaylistFolderRow? = null,
) {
    var name by remember(folder?.folderKey) { mutableStateOf(folder?.name.orEmpty()) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate(if (folder == null) "New folder" else "Rename folder")) },
        text = { OutlinedTextField(name, { name = it }, label = { Text(translate("Folder name")) }, singleLine = true) },
        confirmButton = { TextButton({ model.runAction {
            val source = model.sourceOwner ?: return@runAction
            if (folder == null) createPlaylistFolder(source, name.trim(), smart)
            else renamePlaylistFolder(source, folder.folderKey, name.trim())
            onDismiss()
        } }, enabled = name.isNotBlank() && model.pendingActions == 0) {
            Text(translate(if (folder == null) "Create" else "Save"))
        } }, dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}
