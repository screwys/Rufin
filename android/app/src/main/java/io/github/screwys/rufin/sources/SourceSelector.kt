package io.github.screwys.rufin.sources

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.core.AndroidMusicFolderState
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import kotlinx.coroutines.CancellationException

@Composable
internal fun SourceSelector(expanded: Boolean, model: RufinConnection, browse: BrowseConnection,
    dismiss: () -> Unit, manage: () -> Unit, add: () -> Unit) {
    val sources = model.sources
    val selectedId = sources?.selectedSourceId
    var folders by remember(selectedId) { mutableStateOf<AndroidMusicFolderState?>(null) }
    LaunchedEffect(expanded, browse.library, selectedId, model.libraryState) {
        val library = browse.library ?: return@LaunchedEffect
        if (!expanded || selectedId != model.libraryState?.sourceId) return@LaunchedEffect
        try { folders = library.musicFolders() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { model.runAction { throw failure } }
    }
    DropdownMenu(expanded, dismiss, Modifier.widthIn(min = 220.dp, max = 280.dp).heightIn(max = 480.dp)) {
        Text(translate("Select Source"), Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        if (sources == null) CircularProgressIndicator(Modifier.padding(16.dp).size(24.dp))
        else sources.sources.sortedBy { it.id != selectedId }.forEach { source ->
            val selected = source.id == selectedId
            DropdownMenuItem(text = { Text(source.name) }, onClick = { dismiss(); model.selectSource(source.id) },
                leadingIcon = { SourceIcon(source.kind, Modifier.size(20.dp)) },
                trailingIcon = if (selected) ({ RufinIcon("rufin-object-select-symbolic", null, Modifier.size(16.dp)) }) else null,
                modifier = Modifier.padding(horizontal = 4.dp).background(
                    if (selected) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent, RoundedCornerShape(8.dp))
                    .semantics { this.selected = selected })
        }
        DropdownMenuItem(text = { Text(translate("Manage")) }, onClick = { dismiss(); manage() },
            leadingIcon = { RufinIcon("rufin-document-edit-symbolic", null, Modifier.size(20.dp)) },
            modifier = Modifier.padding(horizontal = 4.dp))
        DropdownMenuItem(text = { Text(translate("Add a new source")) }, onClick = { dismiss(); add() },
            leadingIcon = { RufinIcon("rufin-list-add-symbolic", null, Modifier.size(20.dp)) },
            modifier = Modifier.padding(horizontal = 4.dp))
        folders?.takeIf { it.choices.any { choice -> choice.id != null } }?.let { state ->
            HorizontalDivider(Modifier.padding(horizontal = 12.dp, vertical = 4.dp))
            Text(translate("Server Library"), Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            state.choices.forEach { folder ->
                val selected = folder.id == state.selectedId
                DropdownMenuItem(text = { Text(folder.title) }, onClick = {
                    dismiss()
                    model.runAction { browse.library?.setMusicFolder(folder.id) }
                }, trailingIcon = if (selected) ({ RufinIcon("rufin-object-select-symbolic", null, Modifier.size(16.dp)) }) else null,
                    modifier = Modifier.semantics { this.selected = selected })
            }
        }
    }
}
