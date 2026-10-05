package io.github.screwys.rufin.sources

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.settingsRowShape
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun SourcesPage(model: RufinConnection, onAdd: () -> Unit, onEdit: (String) -> Unit,
    embedded: Boolean = false, connectionsOnly: Boolean = false) {
    val state = model.sources
    var bridge by remember { mutableStateOf<AndroidSourceSetup?>(null) }
    var connections by remember { mutableStateOf<List<AndroidSourceFileConnection>>(emptyList()) }
    var remove by remember { mutableStateOf<Pair<String, String>?>(null) }
    var editingConnection by remember { mutableStateOf<String?>(null) }
    var addingConnection by remember { mutableStateOf(false) }
    LaunchedEffect(model.service, model.ready) {
        val runtime = model.service?.runtime?.value?.getOrNull() ?: return@LaunchedEffect
        val setup = runtime.sourceSetup()
        bridge = setup
        try { connections = withContext(Dispatchers.IO) { setup.fileConnections() } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model.runAction { throw error } }
    }
    LaunchedEffect(state, bridge) { bridge?.let { setup -> connections = withContext(Dispatchers.IO) { setup.fileConnections() } } }
    DisposableEffect(bridge) { val owned = bridge; onDispose { owned?.destroy() } }
    SettingsPage(embedded = embedded) {
        if (!connectionsOnly) {
        SettingsGroup(translate("Sources")) {
        val sources = state?.sources.orEmpty()
        sources.forEachIndexed { index, source -> key(source.id) {
            val selected = state?.selectedSourceId == source.id
            SettingsRow(source.name, summary = source.kind,
                shape = settingsRowShape(index, sources.size + 1), selected = selected,
                leading = { SourceIcon(source.kind, Modifier.size(24.dp)) }, trailing = {
                    Row {
                        IconButton({ model.runAction { model.connectedService().runtime.value!!.getOrThrow().refreshSource(source.id) } }) {
                            RufinIcon("rufin-view-refresh-symbolic", translate("Refresh"), Modifier.size(20.dp))
                        }
                        IconButton({ remove = source.id to source.name }) {
                            RufinIcon("rufin-window-close-symbolic", translate("Forget Source"), Modifier.size(20.dp))
                        }
                        IconButton({ onEdit(source.id) }) { RufinIcon("rufin-go-next-symbolic", translate("Edit Source"), Modifier.size(20.dp)) }
                    }
                }, modifier = Modifier.semantics { this.selected = selected }.clickable { onEdit(source.id) })
        } }
            SettingsRow(translate("Add Source"), shape = settingsRowShape(sources.size, sources.size + 1),
                leading = { RufinIcon("rufin-list-add-symbolic", null, Modifier.size(24.dp)) },
                trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) }, modifier = Modifier.clickable(onClick = onAdd))
        }
        }
        if (connectionsOnly) SettingsGroup {
            SettingsRow(translate("File connection"), leading = { RufinIcon("rufin-list-add-symbolic", null) },
                modifier = Modifier.clickable { addingConnection = true })
        }
        if (connections.isNotEmpty()) SettingsGroup(translate("File connections")) {
        connections.forEach { connection -> key(connection.id) {
            SettingsRow(connection.name, summary = connection.kind,
                leading = { SourceIcon(connection.kind, Modifier.size(24.dp)) },
                trailing = { RufinIcon("rufin-go-next-symbolic", null) }, modifier = Modifier.clickable {
                    if (connectionsOnly) editingConnection = connection.id else onEdit(connection.id)
                })
        } }
        }
    }
    if (addingConnection || editingConnection != null) Dialog(onDismissRequest = { model.closeSourceSetup(); addingConnection = false; editingConnection = null },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        SourceSetupScreen(model, {
            addingConnection = false; editingConnection = null
            model.runAction { bridge?.let { connections = it.fileConnections() } }
        }, editingConnection, connectionsOnly = true)
    }
    remove?.let { (id, name) ->
        AlertDialog(onDismissRequest = { remove = null }, title = { Text(translate("Forget Source")) }, text = { Text(name) },
            confirmButton = { TextButton({
                remove = null
                model.runAction { (bridge ?: error(translate("Source setup is unavailable"))).forget(id) }
            }) { Text(translate("Forget Source")) } }, dismissButton = { TextButton({ remove = null }) { Text(translate("Cancel")) } })
    }
}
