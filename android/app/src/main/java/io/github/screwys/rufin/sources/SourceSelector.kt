package io.github.screwys.rufin.sources

import io.github.screwys.rufin.shared.rufin_core.*

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.draw.drawBehind
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
    dismiss: () -> Unit, manage: () -> Unit, add: () -> Unit,
    /** The width of the row it opens from, which the menu spans. */
    width: androidx.compose.ui.unit.Dp? = null) {
    val sources = model.sources
    val selectedId = sources?.selectedSourceId
    var folders by remember(selectedId) { mutableStateOf<AndroidMusicFolderState?>(null) }
    // Read before the menu opens, so it opens at its full height instead of growing a section after.
    LaunchedEffect(browse.library, sources, model.catalogChanges) {
        val library = browse.library ?: return@LaunchedEffect
        if (selectedId != model.sources?.selectedSourceId) return@LaunchedEffect
        try { folders = library.musicFolders() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { model.runAction { throw failure } }
    }
    // The current source and library carry GTK's mark: an accent line on their leading edge, no tick.
    DropdownMenu(expanded, dismiss, (width?.let { Modifier.width(it) } ?: Modifier.widthIn(min = 220.dp, max = 280.dp)).heightIn(max = 480.dp)) {
        Text(translate("Select Source"), Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        if (sources == null) CircularProgressIndicator(Modifier.padding(16.dp).size(24.dp))
        else sources.sources.sortedBy { it.id != selectedId }.forEach { source ->
            val selected = source.id == selectedId
            DropdownMenuItem(text = { Text(source.name) }, onClick = { dismiss(); if (!selected) model.selectSource(source.id) },
                leadingIcon = { SourceIcon(source.kind, Modifier.size(20.dp)) },
                modifier = Modifier.menuSelected(selected).semantics { this.selected = selected })
        }
        DropdownMenuItem(text = { Text(translate("Manage")) }, onClick = { dismiss(); manage() },
            leadingIcon = { RufinIcon("rufin-document-edit-symbolic", null, Modifier.size(20.dp)) },
            modifier = Modifier.padding(horizontal = 4.dp))
        DropdownMenuItem(text = { Text(translate("Add a new source")) }, onClick = { dismiss(); add() },
            leadingIcon = { RufinIcon("rufin-list-add-symbolic", null, Modifier.size(20.dp)) },
            modifier = Modifier.padding(horizontal = 4.dp))
        folders?.takeIf { it.choices.any { choice -> choice.id != null } }?.let { state ->
            Text(translate("Server Library"), Modifier.padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 8.dp),
                style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            state.choices.forEach { folder ->
                val selected = folder.id == state.selectedId
                DropdownMenuItem(text = { Text(folder.title) }, onClick = {
                    dismiss()
                    model.runAction { browse.library?.setMusicFolder(folder.id) }
                }, modifier = Modifier.menuSelected(selected).semantics { this.selected = selected })
            }
        }
    }
}

/** GTK's mark of a menu's current item (`rufin-menu-selected`): a thin accent line on its leading edge. */
@Composable
private fun Modifier.menuSelected(selected: Boolean): Modifier {
    if (!selected) return this
    val accent = MaterialTheme.colorScheme.primary
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    return drawBehind {
        val line = 2.dp.toPx()
        drawRect(accent, Offset(if (rtl) size.width - line else 0f, 0f), Size(line, size.height))
    }
}
