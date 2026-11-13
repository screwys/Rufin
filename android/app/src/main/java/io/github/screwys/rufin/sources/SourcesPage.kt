package io.github.screwys.rufin.sources

import io.github.screwys.rufin.shared.rufin_core.*

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
import io.github.screwys.rufin.shared.rufin_core.FileIntegration
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.shared.library.ActivityExportFormat
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun SourcesPage(model: RufinConnection, onAdd: () -> Unit, onEdit: (String) -> Unit,
    embedded: Boolean = false, connectionsOnly: Boolean = false) {
    val state = model.sources
    var bridge by remember { mutableStateOf<SourceOwner?>(null) }
    var connections by remember { mutableStateOf<List<FileIntegration>>(emptyList()) }
    var subtitles by remember { mutableStateOf<Map<String, String>?>(null) }
    var editingConnection by remember { mutableStateOf<String?>(null) }
    var addingConnection by remember { mutableStateOf(false) }
    LaunchedEffect(model.service, model.ready) {
        val runtime = model.service?.runtime?.value?.getOrNull() ?: return@LaunchedEffect
        val setup = runtime.source()
        bridge = setup
    }
    LaunchedEffect(state, bridge) {
        try { if (connectionsOnly) bridge?.let { setup -> connections = withContext(Dispatchers.IO) { setup.fileIntegrations().filter { !it.musicSource } } } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model.runAction { throw error } }
    }
    // Each source's lines as GTK shows them: address and account, then cache and file mapping.
    LaunchedEffect(state, bridge) {
        val setup = bridge ?: return@LaunchedEffect
        val configured = state ?: return@LaunchedEffect
        subtitles = configured.sources.associate { source ->
            val access = configured.localAccess.find { it.sourceId == source.id }
            val credentials = try { withContext(Dispatchers.IO) { setup.editable(source.id)?.credentials } }
                catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            val status = if (access?.access == null) null else try { setup.localAccessStatus(source.id) }
                catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            source.id to sourceRowSubtitle(source, access, credentials, status, configured.localFolders.size.toULong())
        }
    }
    DisposableEffect(bridge) { val owned = bridge; onDispose { owned?.destroy() } }
    SettingsPage(embedded = embedded) {
        // The rows wait for their lines, so the page draws once; without a running library they show their names.
        if (!connectionsOnly && (subtitles != null || !model.ready)) {
        SettingsGroup(translate("Sources")) {
        val sources = state?.sources.orEmpty()
        sources.forEachIndexed { index, source -> key(source.id) {
            val selected = state?.selectedSourceId == source.id
            SettingsRow(source.name, summary = subtitles?.get(source.id),
                selected = selected,
                leading = { SourceIcon(source.kind, Modifier.size(24.dp)) }, trailing = {
                    RufinIcon("rufin-go-next-symbolic", translate("Edit Source"), Modifier.size(20.dp))
                }, modifier = Modifier.semantics { this.selected = selected }.clickable { onEdit(source.id) })
        } }
            SettingsRow(translate("Add Source"),
                leading = { RufinIcon("rufin-list-add-symbolic", null, Modifier.size(24.dp)) },
                trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) }, modifier = Modifier.clickable(onClick = onAdd))
        }
        ActivityGroup(model)
        }
        if (connectionsOnly) SettingsGroup(translate("File connections")) {
            SettingsRow(translate("File connection"), leading = { RufinIcon("rufin-list-add-symbolic", null) },
                modifier = Modifier.clickable { addingConnection = true })
        connections.forEach { connection -> key(connection.id) {
            SettingsRow(connection.name, summary = connection.kind,
                leading = { SourceIcon(connection.kind, Modifier.size(24.dp)) },
                trailing = { RufinIcon("rufin-go-next-symbolic", null) }, modifier = Modifier.clickable {
                    editingConnection = connection.id
                })
        } }
        }
    }
    if (addingConnection || editingConnection != null) Dialog(onDismissRequest = { model.closeSourceSetup(); addingConnection = false; editingConnection = null },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        SourceSetupScreen(model, {
            addingConnection = false; editingConnection = null
            model.runAction { bridge?.let { connections = it.fileIntegrations().filter { !it.musicSource } } }
        }, editingConnection, connectionsOnly = true)
    }
}

/// GTK's Activity group: the listening history exports in a chosen format and scope, and a
/// file of listens or play counts imports.
@Composable
private fun ActivityGroup(model: RufinConnection) {
    var format by remember { mutableStateOf(ActivityExportFormat.LAST_FM) }
    var scope by remember { mutableStateOf("") }
    fun withBackups(work: suspend (BackupOwner) -> Unit) = model.runAction {
        model.connectedService().runtime.value!!.getOrThrow().backups().use { work(it) }
    }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { uri ->
        if (uri != null) withBackups { it.exportActivityDocument(uri.toString(), format, scope.ifEmpty { null }) }
    }
    val import = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) withBackups {
            model.showWorking(translate("Matching tracks…"))
            model.showFeedback(it.importActivityDocument(uri.toString()))
        }
    }
    SettingsGroup(translate("Export Activity")) {
        PreferenceChoice(translate("Export format"), format, listOf(ActivityExportFormat.LAST_FM to "Last.fm",
            ActivityExportFormat.LISTEN_BRAINZ to "ListenBrainz", ActivityExportFormat.JSONL to "JSONL")) { format = it }
        PreferenceChoice(translate("Source"), scope, listOf("" to translate("All")) +
            model.sources?.sources.orEmpty().map { it.id to it.name }) { scope = it }
        SettingsRow(translate("Export your listening history in Rufin"), trailing = {
            FilledTonalButton({ export.launch(if (format == ActivityExportFormat.JSONL) "activity.jsonl" else "activity.csv") }) {
                Text(translate("Export"))
            }
        })
    }
    SettingsGroup(translate("Import Activity")) {
        SettingsRow(translate("Listens from Rufin, ListenBrainz or Last.fm, or play counts from other players"), trailing = {
            FilledTonalButton({ import.launch(arrayOf("*/*")) }) { Text(translate("Import")) }
        })
    }
}

/// What GTK's server actions do: use the source, read it again, or forget it after asking.
@Composable
internal fun SourceActions(model: RufinConnection, sourceId: String, name: String, close: () -> Unit) {
    var forgetting by remember { mutableStateOf(false) }
    fun withSource(work: suspend (SourceOwner) -> Unit) = model.runAction {
        model.connectedService().runtime.value!!.getOrThrow().source().use { work(it) }
    }
    SettingsGroup(translate("Server Actions")) {
        if (model.sources?.selectedSourceId != sourceId) SettingsRow(translate("Use This Source"),
            leading = { RufinIcon("rufin-object-select-symbolic", null) },
            modifier = Modifier.clickable { model.selectSource(sourceId); close() })
        SettingsRow(translate("Resync Library"), leading = { RufinIcon("rufin-view-refresh-symbolic", null) },
            modifier = Modifier.clickable { withSource { it.refreshSource(sourceId, false) }; close() })
        SettingsRow(translate("Forget Source"), leading = { RufinIcon("rufin-window-close-symbolic", null) },
            modifier = Modifier.clickable { forgetting = true })
    }
    if (forgetting) AlertDialog(onDismissRequest = { forgetting = false }, title = { Text(translate("Forget Source")) },
        text = { Text(translate("Remove everything Rufin keeps about {name}, including its favorites, ratings and lyrics?").replace("{name}", name)) },
        confirmButton = { TextButton({
            forgetting = false
            withSource { it.forgetSource(sourceId) }
            close()
        }) { Text(translate("Forget Source")) } }, dismissButton = { TextButton({ forgetting = false }) { Text(translate("Cancel")) } })
}
