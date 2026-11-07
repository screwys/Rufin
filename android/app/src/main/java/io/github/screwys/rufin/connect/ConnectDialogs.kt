package io.github.screwys.rufin.connect

import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Action as ConnectAction

import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.ui.RufinIcon

@Composable
internal fun ConnectJoinDialog(connect: ConnectConnection, dismiss: () -> Unit, join: (String) -> Unit) {
    var invitation by remember { mutableStateOf("") }
    LaunchedEffect(connect) { connect.action(ConnectAction.Discover) }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Join profile")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text(translate("Nearby devices"), Modifier.weight(1f))
                IconButton({ connect.action(ConnectAction.Discover) }, enabled = !connect.pending) {
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
internal fun ConnectConfirmationDialog(connect: ConnectConnection) {
    connect.confirmation?.let { confirmation ->
        AlertDialog(onDismissRequest = connect::cancelConfirmation,
            title = { Text(when (confirmation.kind) {
                Confirmation.REPLACE_PROFILE -> translate("Join profile")
            }) }, text = { Text(confirmation.message) },
            confirmButton = { TextButton(connect::confirm) { Text(translate("Continue")) } },
            dismissButton = { TextButton(connect::cancelConfirmation) { Text(translate("Cancel")) } })
    }
}

@Composable
private fun ConnectFolderChoice(name: String, selected: Boolean, leading: @Composable () -> Unit, select: () -> Unit) {
    ListItem(headlineContent = { Text(name) }, leadingContent = leading,
        trailingContent = { RadioButton(selected, null) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier.selectable(selected, role = Role.RadioButton, onClick = select))
}

/** The sync folder: a document tree on this device, or a folder on a saved file connection. */
@Composable
internal fun ConnectFolderDialog(connect: ConnectConnection, current: SyncFolder?, dismiss: () -> Unit) {
    val context = LocalContext.current
    var selected by remember { mutableStateOf((current as? SyncFolder.Connection)?.connection) }
    var path by remember { mutableStateOf((current as? SyncFolder.Connection)?.path.orEmpty()) }
    val chooseFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) try {
            context.contentResolver.takePersistableUriPermission(uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            connect.action(ConnectAction.Folder(SyncFolder.Document(uri.toString())))
            dismiss()
        } catch (error: Exception) { connect.reportError(error) }
    }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Sync folder")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            ConnectFolderChoice(translate("This device"), selected == null,
                { RufinIcon("rufin-phonelink-symbolic", null, Modifier.size(24.dp)) }) { selected = null; path = "" }
            connect.fileSources.forEach { source ->
                ConnectFolderChoice(source.name, source.id == selected,
                    { SourceIcon(source.kind, Modifier.size(24.dp)) }) { selected = source.id; path = "" }
            }
            if (selected != null) OutlinedTextField(path, { path = it }, Modifier.fillMaxWidth(),
                label = { Text(translate("Folder path")) }, singleLine = true)
        }
    }, confirmButton = { TextButton({
        val connection = selected
        if (connection == null) chooseFolder.launch(null)
        else { connect.action(ConnectAction.Folder(SyncFolder.Connection(connection, path.trim()))); dismiss() }
    }, enabled = !connect.pending) { Text(if (selected == null) translate("Choose folder") else translate("Apply")) } },
        dismissButton = { Row {
            if (current != null) TextButton({ connect.action(ConnectAction.Folder(null)); dismiss() },
                enabled = !connect.pending) { Text(translate("Clear")) }
            TextButton(dismiss) { Text(translate("Cancel")) }
        } })
}
