package io.github.screwys.rufin.settings

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import org.json.JSONObject

@Composable
internal fun BackupsPage(model: RufinConnection, preferences: PreferencesConnection,
    embedded: Boolean = false, openSettings: () -> Unit) {
    val scope = rememberCoroutineScope()
    var password by remember { mutableStateOf<String?>(null) }
    var importing by remember { mutableStateOf<Boolean?>(null) }
    var contents by remember { mutableStateOf(defaultBackupContents()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var staged by remember { mutableStateOf<AndroidBackupPreview?>(null) }
    var summary by remember { mutableStateOf<AndroidBackupSummary?>(null) }
    var scheduleError by remember { mutableStateOf<String?>(null) }
    DisposableEffect(Unit) { onDispose { staged?.destroy() } }
    LaunchedEffect(model.ready) {
        if (model.ready) model.connectedService().runtime.value!!.getOrThrow().backups().use { scheduleError = it.scheduleError() }
    }
    fun run(action: suspend (AndroidBackups) -> Unit) { scope.launch {
        busy = true
        message = null
        try { model.connectedService().runtime.value!!.getOrThrow().backups().use { action(it) } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { message = error.message }
        finally { busy = false }
    } }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        if (uri != null) run {
            val selected = preferences.applicationSettings?.optJSONObject("backup")?.optJSONObject("contents") ?: defaultBackupContents()
            it.exportDocument(uri.toString(), password, backupContents(selected))
            password = null
            message = translate("Backup exported")
        }
    }
    val import = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) run { owner ->
            val preview = owner.stageDocument(uri.toString(), password, backupContents(contents))
            password = null
            staged?.destroy()
            staged = preview
            summary = preview.summary()
        }
    }
    SettingsPage(embedded = embedded) {
        if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        message?.let { Text(it) }
        SettingsGroup(translate("Backups")) {
            SettingsLink(null, translate("Automatic Backups"), open = openSettings)
            Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
                Row(Modifier.fillMaxWidth().padding(12.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlinedButton({ contents = defaultBackupContents(); importing = true }, enabled = !busy,
                        modifier = Modifier.weight(1f), colors = settingsButtonColors()) { Text(translate("Import Backup")) }
                    OutlinedButton({ importing = false }, enabled = !busy, modifier = Modifier.weight(1f),
                        colors = settingsButtonColors()) { Text(translate("Export Backup")) }
                }
            }
            scheduleError?.let { SettingsRow(translate("Scheduled Backup Failed"), summary = it) }
        }
    }
    importing?.let { importBackup ->
        BackupPasswordDialog(importBackup, contents, { contents = it }, dismiss = { importing = null }) { passphrase ->
            password = passphrase
            importing = null
            if (importBackup) import.launch(arrayOf("application/octet-stream", "application/zip", "*/*"))
            else export.launch("rufin.rufin-backup")
        }
    }
    summary?.let { preview ->
        fun dismiss() { summary = null; staged?.destroy(); staged = null }
        AlertDialog(onDismissRequest = { if (!busy) dismiss() }, title = { Text(translate("Restore User State")) },
            text = { Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(translate("Replace the selected contents with this backup? Local folder permissions may need to be selected again."))
                Text(translate("Playlists") + ": " + preview.playlistCount)
                preview.removedSources.forEach { Text(it) }
            } },
            confirmButton = { TextButton({ staged?.let { backup -> run { owner ->
                val report = owner.restore(backup)
                preferences.refreshApplicationSettings()
                message = translate("Backup restored") + report.warnings.takeIf { it.isNotEmpty() }?.joinToString("\n", prefix = "\n").orEmpty()
                dismiss()
            } } }, enabled = !busy) { Text(translate("Restore")) } },
            dismissButton = { TextButton({ dismiss() }, enabled = !busy) { Text(translate("Cancel")) } })
    }
}

@Composable
internal fun BackupPasswordDialog(importing: Boolean, contents: JSONObject = defaultBackupContents(),
    changeContents: (JSONObject) -> Unit = {}, dismiss: () -> Unit, continueAction: (String?) -> Unit) {
    var password by remember { mutableStateOf("") }
    var unencrypted by remember { mutableStateOf(false) }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Backup Password")) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedTextField(password, { password = it }, Modifier.fillMaxWidth(), label = { Text(translate("Password")) },
                singleLine = true, enabled = importing || !unencrypted, visualTransformation = PasswordVisualTransformation(),
                colors = settingsTextFieldColors())
            if (importing) {
                Text(translate("Leave empty for an unencrypted archive."), style = MaterialTheme.typography.bodySmall)
                BackupContentsControls(contents, changeContents)
            } else SettingsSwitch(translate("Do not encrypt"), unencrypted) { unencrypted = it }
        }
    }, confirmButton = { TextButton({ continueAction(password.takeIf { it.isNotEmpty() && (importing || !unencrypted) }) },
        enabled = importing || unencrypted || password.isNotEmpty()) { Text(translate("Continue")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
internal fun BackupContentsControls(contents: JSONObject, change: (JSONObject) -> Unit) {
    SettingsGroup(translate("Contents")) {
        listOf("settings" to "Settings and sources", "saved_logins" to "Saved logins", "playlists" to "Playlists",
            "favorites" to "Favorites and ratings", "local_imports" to "Local imports and access",
            "activity" to "Activity", "queue" to "Queue").forEach { (key, label) ->
            SettingsSwitch(translate(label), contents.optBoolean(key, true)) {
                change(JSONObject(contents.toString()).put(key, it))
            }
        }
    }
}

internal fun defaultBackupContents() = JSONObject().apply {
    listOf("settings", "saved_logins", "playlists", "favorites", "local_imports", "activity", "queue").forEach { put(it, true) }
}

private fun backupContents(contents: JSONObject) = AndroidBackupContents(contents.optBoolean("settings", true),
    contents.optBoolean("saved_logins", true), contents.optBoolean("playlists", true), contents.optBoolean("favorites", true),
    contents.optBoolean("local_imports", true), contents.optBoolean("activity", true), contents.optBoolean("queue", true))
