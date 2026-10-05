package io.github.screwys.rufin.controller

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import kotlinx.coroutines.delay

@Composable
internal fun ControllerPage(model: RufinConnection, embedded: Boolean = false) {
    val connection = remember(model) { ControllerConnection(model) }
    LaunchedEffect(connection) { connection.observe() }
    val state = connection.state
    val context = LocalContext.current
    var changePort by rememberSaveable { mutableStateOf(false) }
    var port by rememberSaveable { mutableStateOf("") }
    var regenerate by rememberSaveable { mutableStateOf(false) }
    var tokenCopied by remember { mutableIntStateOf(0) }
    LaunchedEffect(tokenCopied) { if (tokenCopied > 0) { delay(1500); tokenCopied = 0 } }
    fun copy(text: String, label: String) {
        (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
            .setPrimaryClip(ClipData.newPlainText(translate("Controller"), text))
        model.showFeedback(translate(label))
    }
    SettingsPage(embedded = embedded) {
        SettingsGroup {
        SettingsRow(translate("Enable Controller"),
            leading = { RufinIcon("rufin-phonelink-symbolic", null) },
            trailing = { Switch(state?.enabled == true, connection::enable, enabled = state != null) })
        SettingsRow(translate("Allow remote connections"),
            leading = { RufinIcon("rufin-network-workgroup-symbolic", null) },
            trailing = { Switch(state?.remote == true, connection::remote, enabled = state != null) })
        SettingsRow(translate("Port"), summary = state?.port?.toString().orEmpty(),
            trailing = { TextButton({ port = state?.port?.toString().orEmpty(); changePort = true }, enabled = state != null) { Text(translate("Change")) } })
        SettingsRow(translate("Access token"), trailing = {
            Row {
                PlayerIconButton("rufin-view-refresh-symbolic", "Regenerate access token", { regenerate = true }, enabled = state != null)
                PlayerIconButton(if (tokenCopied > 0) "rufin-object-select-symbolic" else "rufin-edit-copy-symbolic", "Copy access token", {
                    connection.copyToken { copy(it, "Copy access token"); tokenCopied++ }
                }, enabled = state != null)
            }
        })
        }
        SettingsGroup(translate("Controller status")) {
        if (state == null) Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        else {
            state.error?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
            Text(state.address ?: if (state.enabled) translate("Starting...") else translate("Disabled"),
                Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            state.links.forEach { link ->
                var copied by remember(link) { mutableIntStateOf(0) }
                LaunchedEffect(copied) { if (copied > 0) { delay(1500); copied = 0 } }
                SettingsRow(link.substringBefore('#'), trailing = {
                    Row {
                        PlayerIconButton("rufin-external-link-symbolic", "Open Controller", {
                            connection.action("Open Controller") { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(link))) }
                        })
                        PlayerIconButton(if (copied > 0) "rufin-object-select-symbolic" else "rufin-edit-copy-symbolic", "Copy link", {
                            connection.action { copy(link, "Copy link"); copied++ }
                        })
                        PlayerIconButton("rufin-document-send-symbolic", "Share", {
                            connection.action("Share") { context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain")
                                .putExtra(Intent.EXTRA_TEXT, link), translate("Controller"))) }
                        })
                    }
                })
            }
        }
        }
    }
    if (changePort) AlertDialog(onDismissRequest = { changePort = false }, title = { Text(translate("Port")) },
        text = { OutlinedTextField(port, { port = it }, singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number)) },
        confirmButton = { TextButton({ port.toUShortOrNull()?.let(connection::port); changePort = false }, enabled = port.toUShortOrNull() != null) { Text(translate("Save")) } },
        dismissButton = { TextButton({ changePort = false }) { Text(translate("Cancel")) } })
    if (regenerate) AlertDialog(onDismissRequest = { regenerate = false }, title = { Text(translate("Regenerate access token?")) },
        text = { Text(translate("You will need to reconnect through the clients you use with the new API key.")) },
        confirmButton = { TextButton({ regenerate = false; connection.regenerateToken() }) { Text(translate("Regenerate access token")) } },
        dismissButton = { TextButton({ regenerate = false }) { Text(translate("Cancel")) } })
}
