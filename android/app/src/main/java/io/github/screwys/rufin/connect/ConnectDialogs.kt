package io.github.screwys.rufin.connect

import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.CancellationException
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.ui.RufinIcon

@Composable
internal fun ConnectJoinDialog(connect: ConnectConnection, dismiss: () -> Unit, join: (String) -> Unit) {
    var invitation by remember { mutableStateOf("") }
    LaunchedEffect(connect) { connect.action(AndroidConnectAction.Discover) }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Join profile")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text(translate("Nearby devices"), Modifier.weight(1f))
                IconButton({ connect.action(AndroidConnectAction.Discover) }, enabled = !connect.pending) {
                    RufinIcon("rufin-view-refresh-symbolic", translate("Refresh"))
                }
            }
            val devices = connect.state?.devices.orEmpty().filter { !it.enrolled }
            if (devices.isEmpty()) Text(translate("No network devices found"), color = MaterialTheme.colorScheme.onSurfaceVariant)
            devices.forEach { device ->
                key(device.id) {
                    ListItem(headlineContent = { Text(device.name) },
                        trailingContent = { TextButton({ join(device.id) }, enabled = !connect.pending) { Text(translate("Join profile")) } },
                        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
                        modifier = Modifier.clickable(enabled = !connect.pending) { join(device.id) })
                }
            }
            OutlinedTextField(invitation, { invitation = it }, Modifier.fillMaxWidth(),
                label = { Text(translate("Device invitation")) }, singleLine = true)
        }
    }, confirmButton = { TextButton({ join(invitation) }, enabled = invitation.isNotBlank() && !connect.pending) { Text(translate("Join profile")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
internal fun ConnectTextDialog(title: String, label: String, initial: String, dismiss: () -> Unit,
    secret: Boolean = false, allowEmpty: Boolean = false, acceptTitle: String = translate("Apply"), accept: (String) -> Unit) {
    var text by remember { mutableStateOf(initial) }
    AlertDialog(onDismissRequest = dismiss, title = { Text(title) }, text = {
        OutlinedTextField(text, { text = it }, Modifier.fillMaxWidth(), label = { Text(label) }, singleLine = true,
            visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None)
    }, confirmButton = { TextButton({ accept(text) }, enabled = allowEmpty || text.isNotBlank()) { Text(acceptTitle) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
internal fun ConnectStorageDialog(current: AndroidConnectDestination?, dismiss: () -> Unit,
    useDefault: () -> Unit, chooseFolder: () -> Unit, chooseConnection: () -> Unit) {
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Storage")) }, text = {
        Column {
            listOf(
                Triple(translate("Default"), current == null, useDefault),
                Triple(translate("This device"), current is AndroidConnectDestination.Document || current is AndroidConnectDestination.Local, chooseFolder),
                Triple(translate("File connection"), current is AndroidConnectDestination.Remote, chooseConnection),
            ).forEach { (title, selected, choose) ->
                ListItem(headlineContent = { Text(title) }, trailingContent = { RadioButton(selected, null) },
                    colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
                    modifier = Modifier.selectable(selected, role = Role.RadioButton) { dismiss(); choose() })
            }
        }
    }, confirmButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
internal fun ConnectConfirmationDialog(connect: ConnectConnection) {
    connect.confirmation?.let { confirmation ->
        AlertDialog(onDismissRequest = connect::cancelConfirmation,
            title = { Text(when (confirmation.kind) {
                AndroidConnectConfirmation.REPLACE_PROFILE -> translate("Join profile")
                AndroidConnectConfirmation.REDOWNLOAD_FILES -> translate("Download quality")
            }) }, text = { Text(confirmation.message) },
            confirmButton = { TextButton(connect::confirm) { Text(translate("Continue")) } },
            dismissButton = { TextButton(connect::cancelConfirmation) { Text(translate("Cancel")) } })
    }
}

@Composable
internal fun ConnectRemoteStorageDialog(connect: ConnectConnection, current: AndroidConnectDestination?,
    dismiss: () -> Unit, accept: (AndroidConnectDestination.Remote) -> Unit) {
    var selected by remember { mutableStateOf((current as? AndroidConnectDestination.Remote)?.sourceId ?: connect.fileSources.firstOrNull()?.id) }
    val source = connect.fileSources.firstOrNull { it.id == selected }
    var path by remember(selected) { mutableStateOf(
        (current as? AndroidConnectDestination.Remote)?.takeIf { it.sourceId == selected }?.path ?: source?.path.orEmpty()
    ) }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Profile storage")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            ConnectFileSourceChoices(connect, selected) { selected = it }
            OutlinedTextField(path, { path = it }, Modifier.fillMaxWidth(), label = { Text(translate("Folder path")) }, singleLine = true)
        }
    }, confirmButton = { TextButton({ selected?.let { accept(AndroidConnectDestination.Remote(it, path)) } },
        enabled = selected != null && !connect.pending) { Text(translate("Apply")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
private fun ConnectFileSourceChoices(connect: ConnectConnection, selected: String?, select: (String) -> Unit) {
    if (connect.fileSources.isEmpty()) Text(translate("No file connections"), Modifier.padding(vertical = 12.dp))
    connect.fileSources.forEach { source ->
        ListItem(headlineContent = { Text(source.name) },
            leadingContent = { SourceIcon(source.kind, Modifier.size(24.dp)) },
            trailingContent = { RadioButton(source.id == selected, null) },
            colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
            modifier = Modifier.selectable(source.id == selected, role = Role.RadioButton) { select(source.id) })
    }
}

@Composable
internal fun ConnectRemoteFileDialog(connect: ConnectConnection, importing: Boolean, dismiss: () -> Unit) {
    var selected by remember { mutableStateOf(connect.fileSources.firstOrNull()?.id) }
    val source = connect.fileSources.firstOrNull { it.id == selected }
    var folder by remember(selected) { mutableStateOf(source?.path.orEmpty()) }
    var files by remember { mutableStateOf<List<String>>(emptyList()) }
    var path by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(false) }
    var failure by remember { mutableStateOf<String?>(null) }
    var lookup by remember { mutableIntStateOf(0) }
    LaunchedEffect(selected, lookup) {
        if (!importing || lookup == 0) return@LaunchedEffect
        files = emptyList()
        failure = null
        val id = selected ?: return@LaunchedEffect
        val location = folder
        loading = true
        try {
            val loaded = connect.profileFiles(id, location)
            if (selected == id && folder == location) files = loaded
        }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error.message }
        finally { loading = false }
    }
    AlertDialog(onDismissRequest = dismiss, title = { Text(if (importing) translate("Import") else translate("Export")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            ConnectFileSourceChoices(connect, selected) { selected = it; path = "" }
            if (importing) {
                OutlinedTextField(folder, { folder = it; files = emptyList(); path = "" }, Modifier.fillMaxWidth(), label = { Text(translate("Folder path")) }, singleLine = true)
                TextButton({ lookup++ }, enabled = selected != null && !loading) { Text(translate("Browse")) }
                Box(Modifier.fillMaxWidth().height(2.dp)) { if (loading) LinearProgressIndicator(Modifier.fillMaxWidth()) }
            }
            failure?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
            if (importing) files.forEach { file ->
                ListItem(headlineContent = { Text(file.substringAfterLast('/')) },
                    leadingContent = { RadioButton(path == file, { path = file }) },
                    modifier = Modifier.clickable { path = file })
            }
            OutlinedTextField(path, { path = it }, Modifier.fillMaxWidth(), label = { Text(translate("Connect file")) }, singleLine = true)
        }
    }, confirmButton = { TextButton({ selected?.let { sourceId ->
        if (importing) connect.importRemote(sourceId, path)
        else connect.exportRemote(sourceId, path)
        dismiss()
    } }, enabled = selected != null && path.isNotBlank() && !connect.pending) { Text(if (importing) translate("Import") else translate("Export")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}
