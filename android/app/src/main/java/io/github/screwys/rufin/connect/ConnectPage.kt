package io.github.screwys.rufin.connect

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Action as ConnectAction
import io.github.screwys.rufin.shared.rufin_core.Status as ConnectStatus

import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import java.text.NumberFormat

@Composable
internal fun ConnectProgressScreen(model: RufinConnection) {
    Surface(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(model.connect::dismissOperation) { RufinIcon("rufin-go-previous-symbolic", translate("Back")) }
                Text(translate("Rufin Connect"), Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
            }
            Box(Modifier.weight(1f)) { ConnectPage(model, progressOnly = true) }
        }
    }
}

@Composable
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal fun ConnectPage(model: RufinConnection, embedded: Boolean = false, progressOnly: Boolean = false) {
    val connect = model.connect
    DisposableEffect(connect) { connect.pageVisible = true; onDispose { connect.pageVisible = false } }
    val state = connect.state
    LaunchedEffect(state?.settings?.profile, state?.settings?.enabled) {
        if (!progressOnly && state?.settings?.profile != null && state.settings.enabled) {
            if (!model.connectedService().requestLocalNetworkAccess()) {
                model.showFeedback(translate("Local network permission was denied"))
            }
        }
    }
    var join by remember { mutableStateOf(false) }
    var rename by remember { mutableStateOf(false) }
    var leave by remember { mutableStateOf(false) }
    var disable by remember { mutableStateOf(false) }
    var relayEdit by remember { mutableStateOf(false) }
    var syncFolder by remember { mutableStateOf(false) }
    var controlDevice by remember { mutableStateOf<Device?>(null) }
    var removeDevice by remember { mutableStateOf<Device?>(null) }
    LaunchedEffect(connect, state != null) {
        if (state?.settings?.nearby == true && state.identity == null) connect.action(ConnectAction.Discover)
    }
    if (state == null) {
        Box(if (embedded) Modifier.fillMaxWidth().padding(24.dp) else Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            val failure = connect.error
            if (failure != null) Text(failure, Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error)
            else CircularProgressIndicator()
        }
        return
    }
    val enabled = !connect.pending
    val failure = connect.visibleError
    val working = failure == null && state.pairing == null &&
        (state.connecting || (state.collectionProgress != null && connect.pending))
    if (working) {
        Box(if (embedded) Modifier.fillMaxWidth().padding(vertical = 48.dp) else Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Column(Modifier.widthIn(max = 420.dp).fillMaxWidth().padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(24.dp)) {
                ConnectProgress(state)
                if (state.connecting && !state.receivingCollection) TextButton({ connect.action(ConnectAction.CancelPairing) }) {
                    Text(translate("Cancel"))
                }
            }
        }
    } else SettingsPage(embedded = embedded) {
        SettingsGroup {
            if (!progressOnly) {
            SettingsRow(translate("Device name"), summary = state.settings.name,
                trailing = { IconButton({ rename = true }, enabled = enabled) { RufinIcon("rufin-document-edit-symbolic", translate("Edit")) } },
                modifier = Modifier.clickable(enabled) { rename = true })
            if (state.settings.profile == null) {
                Button({ connect.action(ConnectAction.Enable(true)) }, Modifier.fillMaxWidth(), enabled = enabled) {
                    Text(translate("Enable Rufin Connect"))
                }
            } else SettingsRow(translate("Enable Rufin Connect"), summary = state.profileStatus.takeIf { failure == null },
                trailing = { Switch(state.settings.enabled, {
                    if (it) connect.action(ConnectAction.Enable(true)) else disable = true
                }, enabled = enabled) })
            }
            if (progressOnly && failure == null) Text(state.profileStatus, Modifier.padding(horizontal = 12.dp, vertical = 8.dp), style = MaterialTheme.typography.titleMedium)
            if (connect.pending) LinearProgressIndicator(Modifier.fillMaxWidth())
            failure?.let { message ->
                Text(message, Modifier.padding(horizontal = 16.dp, vertical = 8.dp), color = MaterialTheme.colorScheme.error)
                if (state.settings.profile != null && !state.connecting) TextButton({ connect.action(ConnectAction.Retry) }, enabled = enabled) {
                    Text(translate("Retry"))
                }
            }
        }
        if (!progressOnly) {
        SettingsGroup {
            SettingsRow(translate("Join profile"),
                modifier = Modifier.clickable(enabled) { join = true })
            if (state.settings.profile != null) {
                state.invitation?.let { invitation ->
                    SettingsRow(translate("Connect a device"),
                        trailing = { io.github.screwys.rufin.ui.CopyIconButton("Copy invitation", sensitive = true,
                            clipboardLabel = translate("Device invitation")) { copy -> copy(invitation) } })
                }
                if (state.devices.any { it.enrolled }) SettingsRow(translate("Disconnect this device"),
                    modifier = Modifier.clickable(enabled) { leave = true })
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth().padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Nearby devices"), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
                IconButton({ connect.action(ConnectAction.Discover) }, enabled = enabled) {
                    RufinIcon("rufin-view-refresh-symbolic", translate("Refresh"))
                }
            }
            val nearby = state.devices.filter { !it.enrolled }
            if (nearby.isEmpty()) Text(translate("No network devices found"), Modifier.padding(horizontal = 12.dp, vertical = 8.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
            SettingsGroup { nearby.forEach { device ->
                key(device.id) { ConnectDeviceRow(device, connect, { controlDevice = device }, { removeDevice = device }) }
            } }
        }
        val connected = state.devices.filter { it.enrolled }
        if (connected.isNotEmpty()) Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth().padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Connected devices"), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
                IconButton({ connect.action(ConnectAction.Refresh) }, enabled = enabled) { RufinIcon("rufin-view-refresh-symbolic", translate("Refresh")) }
            }
            SettingsGroup { connected.forEach { device ->
                key(device.id) { ConnectDeviceRow(device, connect, { controlDevice = device }, { removeDevice = device }) }
            } }
        }
        SettingsGroup(translate("Connection settings")) {
            SettingsRow(translate("Nearby discovery"),
                trailing = { Switch(state.settings.nearby, { connect.action(ConnectAction.Network(it, state.settings.relay)) }, enabled = enabled) })
            SettingsRow(translate("Relay address"), summary = state.settings.relay ?: translate("Off"),
                modifier = Modifier.clickable(enabled) { relayEdit = true })
        }
        }
        if (!progressOnly && state.settings.profile != null) {
        SettingsGroup {
            SettingsRow(translate("Sync folder"), summary = syncFolderSummary(state.settings.folder, connect.fileSources),
                modifier = Modifier.clickable(enabled) { syncFolder = true })
        }
        }
    }
    if (join) ConnectJoinDialog(connect, { join = false }) { connect.join(it); join = false }
    if (relayEdit) ConnectTextDialog(translate("Relay address"), translate("Relay address"), state.settings.relay.orEmpty(), { relayEdit = false }, allowEmpty = true) {
        connect.action(ConnectAction.Network(state.settings.nearby, it.trim().takeIf(String::isNotEmpty))); relayEdit = false
    }
    if (rename) ConnectTextDialog(translate("Device name"), translate("Name"), state.settings.name, { rename = false }) { connect.action(ConnectAction.Rename(it)); rename = false }
    if (disable) AlertDialog(onDismissRequest = { disable = false }, title = { Text(translate("Rufin Connect")) },
        text = { Text(translate("This will remove all configured state. Do you want to continue?")) },
        confirmButton = { TextButton({ connect.action(ConnectAction.Enable(false)); disable = false }) { Text(translate("Continue")) } },
        dismissButton = { TextButton({ disable = false }) { Text(translate("Cancel")) } })
    if (leave) AlertDialog(onDismissRequest = { leave = false }, title = { Text(translate("Disconnect this device?")) },
        confirmButton = { TextButton({ connect.action(ConnectAction.Enable(false)); leave = false }) { Text(translate("Disconnect")) } },
        dismissButton = { TextButton({ leave = false }) { Text(translate("Cancel")) } })
    removeDevice?.let { device -> AlertDialog(onDismissRequest = { removeDevice = null }, title = { Text(translate("Remove device")) }, text = { Text(device.name) },
        confirmButton = { TextButton({ connect.action(ConnectAction.Remove(device.id)); removeDevice = null }) { Text(translate("Remove")) } },
        dismissButton = { TextButton({ removeDevice = null }) { Text(translate("Cancel")) } }) }
    if (syncFolder) ConnectFolderDialog(connect, state.settings.folder) { syncFolder = false }
    controlDevice?.let { device -> ConnectDeviceControls(device, connect) { controlDevice = null } }
    ConnectConfirmationDialog(connect)
}

@Composable
private fun ConnectProgress(state: ConnectStatus) {
    val progress = state.collectionProgress
    val total = progress?.total?.takeIf { it > 0uL }
    val fraction = if (progress != null && total != null) (progress.completed.toDouble() / total.toDouble()).coerceIn(0.0, 1.0) else null
    val integer = remember { NumberFormat.getIntegerInstance() }
    val counter = progress?.let {
        val current = integer.format(it.completed.toLong())
        if (total != null && fraction != null) {
            val percent = integer.format((it.completed.coerceAtMost(total).toDouble() * 100 / total.toDouble()).toInt())
            "$current / ${integer.format(total.toLong())} ($percent%)"
        } else if (it.completed > 0uL) current else null
    }
    Text(state.profileStatus, Modifier.fillMaxWidth(), textAlign = TextAlign.Center, style = MaterialTheme.typography.titleMedium)
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        if (fraction == null) LinearProgressIndicator(Modifier.fillMaxWidth().height(8.dp))
        else LinearProgressIndicator(progress = { fraction.toFloat() }, modifier = Modifier.fillMaxWidth().height(8.dp))
        counter?.let { Text(it, textAlign = TextAlign.Center, style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}

private fun syncFolderSummary(folder: SyncFolder?, connections: List<FileIntegration>): String? = when (folder) {
    is SyncFolder.Connection -> listOfNotNull(connections.firstOrNull { it.id == folder.connection }?.name,
        folder.path.takeIf(String::isNotEmpty)).joinToString(" · ")
    is SyncFolder.Document -> android.net.Uri.parse(folder.document).lastPathSegment ?: folder.document
    is SyncFolder.Path -> folder.path
    null -> null
}
