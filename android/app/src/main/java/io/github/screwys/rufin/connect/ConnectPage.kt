package io.github.screwys.rufin.connect

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.PersistableBundle
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import kotlinx.coroutines.delay

@Composable
internal fun ConnectSetupScreen(model: RufinConnection) {
    Surface(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Rufin Connect"), Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                TextButton({ model.connect.action(AndroidConnectAction.CancelPairing) }) { Text(translate("Cancel")) }
            }
            Box(Modifier.weight(1f)) { ConnectPage(model, setupOnly = true) }
        }
    }
}

@Composable
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal fun ConnectPage(model: RufinConnection, embedded: Boolean = false, setupOnly: Boolean = false) {
    val connect = model.connect
    DisposableEffect(connect) { connect.pageVisible = true; onDispose { connect.pageVisible = false } }
    val state = connect.state
    LaunchedEffect(state?.profile, state?.enabled) {
        if (!setupOnly && state?.profile != null && state.enabled) {
            if (!model.connectedService().requestLocalNetworkAccess()) {
                model.showFeedback(translate("Local network permission was denied"))
            }
            connect.action(AndroidConnectAction.Refresh)
        }
    }
    val context = LocalContext.current
    var join by remember { mutableStateOf(false) }
    var rename by remember { mutableStateOf(false) }
    var leave by remember { mutableStateOf(false) }
    var disable by remember { mutableStateOf(false) }
    var remoteStorage by remember { mutableStateOf(false) }
    var storageChoice by remember { mutableStateOf(false) }
    var musicFolderChoice by remember { mutableStateOf<AndroidConnectMusicFolder?>(null) }
    var relayEdit by remember { mutableStateOf(false) }
    var remoteFile by remember { mutableStateOf<Boolean?>(null) }
    var controlDevice by remember { mutableStateOf<AndroidConnectDevice?>(null) }
    var removeDevice by remember { mutableStateOf<AndroidConnectDevice?>(null) }
    var invitationCopied by remember { mutableIntStateOf(0) }
    LaunchedEffect(invitationCopied) { if (invitationCopied > 0) { delay(1500); invitationCopied = 0 } }
    LaunchedEffect(connect, state != null) {
        if (state?.nearby == true && state.identity == null) connect.action(AndroidConnectAction.Discover)
    }
    LaunchedEffect(connect, state?.profile, state?.setupPending, state?.connecting, model.sources) {
        if (state?.profile != null && !state.connecting) connect.refreshMusicFolders()
    }
    val chooseMusicFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        val folder = musicFolderChoice
        if (uri != null && folder != null) try {
            context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            connect.folder(folder, uri.toString())
        } catch (failure: Exception) { connect.reportError(failure) }
        musicFolderChoice = null
    }
    val chooseFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) try {
            context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            connect.action(AndroidConnectAction.Destination(AndroidConnectDestination.Document(uri.toString())))
        } catch (failure: Exception) { connect.reportError(failure) }
    }
    val importFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { connect.importDocument(it.toString()) }
    }
    val exportFile = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        uri?.let { connect.exportDocument(it.toString()) }
    }
    if (state == null) {
        Box(if (embedded) Modifier.fillMaxWidth().padding(24.dp) else Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            val failure = connect.error
            if (failure != null) io.github.screwys.rufin.ui.ErrorNotice(failure)
            else CircularProgressIndicator()
        }
        return
    }
    val enabled = !connect.pending
    SettingsPage(embedded = embedded) {
        SettingsGroup {
            if (!setupOnly) {
            SettingsRow(translate("Device name"), summary = state.name,
                trailing = { IconButton({ rename = true }, enabled = enabled) { RufinIcon("rufin-document-edit-symbolic", translate("Edit")) } },
                modifier = Modifier.clickable(enabled) { rename = true })
            if (state.profile == null) {
                Button({ connect.action(AndroidConnectAction.Create) }, Modifier.fillMaxWidth(), enabled = enabled) {
                    Text(translate("Enable Rufin Connect"))
                }
            } else SettingsRow(translate("Enable Rufin Connect"), summary = state.profileStatus,
                trailing = { Switch(state.enabled, {
                    if (it) connect.action(AndroidConnectAction.Enable(true)) else disable = true
                }, enabled = enabled) })
            }
            if (setupOnly) Text(state.profileStatus, Modifier.padding(horizontal = 12.dp, vertical = 8.dp), style = MaterialTheme.typography.titleMedium)
            Box(Modifier.fillMaxWidth().height(2.dp)) {
                if (connect.pending || state.connecting || state.adopting || state.receivingCollection) LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            if (!setupOnly && state.connecting && state.pairing == null) SettingsRow(state.profileStatus,
                trailing = { TextButton({ connect.action(AndroidConnectAction.CancelPairing) }, enabled = enabled) {
                    Text(translate("Cancel"))
                } })
            connect.visibleError?.let { message ->
                io.github.screwys.rufin.ui.ErrorNotice(message)
            }
        }
        if (!setupOnly) {
        SettingsGroup {
            SettingsRow(translate("Join profile"),
                modifier = Modifier.clickable(enabled) { join = true })
            if (state.profile != null) {
                state.invitation?.let { invitation ->
                    SettingsRow(translate("Connect a device"),
                        trailing = { IconButton({
                            val clip = ClipData.newPlainText(translate("Device invitation"), invitation)
                            if (Build.VERSION.SDK_INT >= 33) {
                                clip.description.extras = PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
                            }
                            (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(clip)
                            invitationCopied++
                        }) { RufinIcon(if (invitationCopied > 0) "rufin-object-select-symbolic" else "rufin-edit-copy-symbolic", translate("Copy invitation")) } })
                }
                if (state.devices.any { it.enrolled }) SettingsRow(translate("Disconnect this device"),
                    modifier = Modifier.clickable(enabled) { leave = true })
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth().padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Nearby devices"), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
                IconButton({ connect.action(AndroidConnectAction.Discover) }, enabled = enabled) {
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
                IconButton({ connect.action(AndroidConnectAction.Refresh) }, enabled = enabled) { RufinIcon("rufin-view-refresh-symbolic", translate("Refresh")) }
            }
            SettingsGroup { connected.forEach { device ->
                key(device.id) { ConnectDeviceRow(device, connect, { controlDevice = device }, { removeDevice = device }) }
            } }
        }
        SettingsGroup(translate("Connection settings")) {
            SettingsRow(translate("Nearby discovery"),
                trailing = { Switch(state.nearby, { connect.action(AndroidConnectAction.Network(it, state.relay, state.publicRelay)) }, enabled = enabled) })
            SettingsRow(translate("Relay address"), summary = state.relay ?: translate("Default"),
                modifier = Modifier.clickable(enabled) { relayEdit = true })
            SettingsRow(translate("Use a public relay"),
                trailing = { Switch(state.publicRelay, { connect.action(AndroidConnectAction.Network(state.nearby, state.relay, it)) }, enabled = enabled) })
        }
        }
        if (!setupOnly || (state.setupPending && !state.connecting)) {
        if (connect.musicFolders.isNotEmpty()) SettingsGroup(translate("Local music folders")) {
            connect.musicFolders.forEach { folder ->
                key(folder.sourceId, folder.rootId) {
                    SettingsRow(folder.title, summary = folder.location ?: translate("Rufin data folder"),
                        trailing = { if (folder.location != null) TextButton({ connect.folder(folder, null) }, enabled = enabled) { Text(translate("Default")) } },
                        modifier = Modifier.clickable(enabled) {
                            musicFolderChoice = folder
                            chooseMusicFolder.launch(folder.location?.takeIf { it.startsWith("content://") }?.let(android.net.Uri::parse))
                        })
                }
            }
        }
        SettingsGroup(translate("Music on this device")) {
            Text(translate("Download quality"), Modifier.padding(horizontal = 12.dp, vertical = 8.dp), style = MaterialTheme.typography.bodyLarge)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(state.encoding == AndroidConnectEncoding.ORIGINAL, { connect.encoding(AndroidConnectEncoding.ORIGINAL) },
                    label = { Text(translate("Original")) }, enabled = enabled)
                FilterChip(state.encoding == AndroidConnectEncoding.MP3, { connect.encoding(AndroidConnectEncoding.MP3) },
                    label = { Text(translate("MP3")) }, enabled = enabled)
            }
            state.mediaStatus?.let { Text(it, Modifier.padding(horizontal = 12.dp, vertical = 8.dp), style = MaterialTheme.typography.bodySmall) }
        }
        SettingsGroup(translate("Profile storage")) {
            SettingsRow(translate("Storage"), summary = when (val destination = state.destination) {
                    is AndroidConnectDestination.Document -> translate("This device")
                    is AndroidConnectDestination.Local -> destination.path
                    is AndroidConnectDestination.Remote -> listOf(connect.fileSources.firstOrNull { it.id == destination.sourceId }?.name.orEmpty(), destination.path).filter(String::isNotBlank).joinToString(" · ")
                    else -> translate("Rufin data folder")
                },
                modifier = Modifier.clickable(enabled) { storageChoice = true })
        }
        }
        if (state.setupPending && !state.connecting) Button({ connect.action(AndroidConnectAction.FinishSetup) },
            Modifier.fillMaxWidth(), enabled = enabled) { Text(translate("Continue")) }
        if (!setupOnly) {
        SettingsGroup {
            SettingsRow(translate("Import"),
                modifier = Modifier.clickable(enabled) { importFile.launch(arrayOf("*/*")) })
            SettingsRow(translate("Export"),
                modifier = Modifier.clickable(enabled && state.profile != null) { exportFile.launch("Rufin.rufin-connect") })
            SettingsRow(translate("Import from connection"), modifier = Modifier.clickable(enabled) { remoteFile = true })
            SettingsRow(translate("Export to connection"), modifier = Modifier.clickable(enabled && state.profile != null) { remoteFile = false })
        }
        }
    }
    if (join) ConnectJoinDialog(connect, { join = false }) { connect.join(it); join = false }
    if (relayEdit) ConnectTextDialog(translate("Relay address"), translate("Relay address"), state.relay.orEmpty(), { relayEdit = false }, allowEmpty = true) {
        connect.action(AndroidConnectAction.Network(state.nearby, it.trim().takeIf(String::isNotEmpty), state.publicRelay)); relayEdit = false
    }
    if (rename) ConnectTextDialog(translate("Device name"), translate("Name"), state.name, { rename = false }) { connect.action(AndroidConnectAction.Rename(it)); rename = false }
    if (disable) AlertDialog(onDismissRequest = { disable = false }, title = { Text(translate("Rufin Connect")) },
        text = { Text(translate("This will remove all configured state. Do you want to continue?")) },
        confirmButton = { TextButton({ connect.action(AndroidConnectAction.Enable(false)); disable = false }) { Text(translate("Continue")) } },
        dismissButton = { TextButton({ disable = false }) { Text(translate("Cancel")) } })
    if (leave) AlertDialog(onDismissRequest = { leave = false }, title = { Text(translate("Disconnect this device?")) },
        confirmButton = { TextButton({ connect.action(AndroidConnectAction.Leave); leave = false }) { Text(translate("Disconnect")) } },
        dismissButton = { TextButton({ leave = false }) { Text(translate("Cancel")) } })
    removeDevice?.let { device -> AlertDialog(onDismissRequest = { removeDevice = null }, title = { Text(translate("Remove device")) }, text = { Text(device.name) },
        confirmButton = { TextButton({ connect.action(AndroidConnectAction.Remove(device.id)); removeDevice = null }) { Text(translate("Remove")) } },
        dismissButton = { TextButton({ removeDevice = null }) { Text(translate("Cancel")) } }) }
    if (remoteStorage) ConnectRemoteStorageDialog(connect, state.destination, { remoteStorage = false }) { destination ->
        connect.action(AndroidConnectAction.Destination(destination)); remoteStorage = false
    }
    if (storageChoice) ConnectStorageDialog(state.destination, { storageChoice = false },
        useDefault = { connect.action(AndroidConnectAction.Destination(null)) },
        chooseFolder = { chooseFolder.launch((state.destination as? AndroidConnectDestination.Document)?.uri?.let(android.net.Uri::parse)) },
        chooseConnection = { remoteStorage = true })
    remoteFile?.let { importing -> ConnectRemoteFileDialog(connect, importing, { remoteFile = null }) }
    controlDevice?.let { device -> ConnectDeviceControls(device, connect) { controlDevice = null } }
    ConnectConfirmationDialog(connect)
}
