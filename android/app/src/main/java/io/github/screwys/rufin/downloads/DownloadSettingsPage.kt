package io.github.screwys.rufin.downloads

import android.app.Activity
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.shared.downloads.*
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow

@Composable
internal fun DownloadSettingsPage(model: RufinConnection, embedded: Boolean = false) {
    val downloads = model.downloads
    val translation = LocalTranslationRevision.current
    LaunchedEffect(translation) { downloads.refreshSources() }
    val sources = downloads.sources
    fun DownloadSource.directory() = when (val directory = settings.directory) { is DownloadDirectory.Native -> directory.v1; is DownloadDirectory.Document -> directory.uri; null -> null }
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    val source = sources.find { it.source.id == selected } ?: sources.find { it.source.id == model.sources?.selectedSourceId } ?: sources.firstOrNull()
    var removingRule by remember { mutableStateOf<Pair<String, DownloadRule>?>(null) }
    var removeAll by remember { mutableStateOf<DownloadSource?>(null) }
    val context = LocalContext.current
    var folderSource by rememberSaveable { mutableStateOf<String?>(null) }
    val folder = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == Activity.RESULT_OK) {
            val uri = result.data?.data
            val sourceId = folderSource
            if (uri != null && sourceId != null) model.runAction {
                val granted = result.data!!.flags
                val read = granted and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0
                val write = granted and Intent.FLAG_GRANT_WRITE_URI_PERMISSION != 0
                when {
                    read && write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                    read -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                    write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                }
                downloads.directory(sourceId, uri.toString())
            }
        }
        folderSource = null
    }
    SettingsPage(embedded = embedded) {
        if (sources.isEmpty()) Text(translate("Add a source to configure downloads."))
        else if (source != null) {
            SettingsGroup {
                PreferenceChoice(translate("Source"), source.source.id, sources.map { it.source.id to it.source.name }, select = { selected = it })
            }
            SettingsGroup(translate("Storage")) {
            SettingsRow(translate("Download Folder"), summary = source.directory() ?: translate("Default"), trailing = {
                TextButton({ folderSource = source.source.id; folder.launch(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)) }) { Text(translate("Choose")) }
            })
            if (source.directory() != null) TextButton({ downloads.directory(source.source.id, null) }) { Text(translate("Use default folder")) }
            DownloadQualityControls(source.settings.quality) { downloads.quality(source.source.id, it) }
            }
            SettingsGroup {
            DownloadRule.entries.forEach { rule -> SettingsRow(rule.title(), trailing = {
                Switch(source.settings.rules.enabled(rule), { enabled -> if (enabled) downloads.rule(source.source.id, rule, true) else removingRule = source.source.id to rule })
            }) }
            }
            SettingsGroup(translate("Downloaded")) {
            SettingsRow(translate("Remove All Downloads"),
                summary = trackCountText(downloads.state?.queues?.find { it.sourceId == source.source.id }?.downloadedTracks ?: 0UL),
                trailing = { TextButton({ removeAll = source }) { Text(translate("Remove")) } })
            }
        }
    }
    removingRule?.let { (sourceId, rule) -> AlertDialog(onDismissRequest = { removingRule = null }, title = { Text(rule.title()) },
        text = { Text(translate("Choose whether to keep the completed downloads.")) },
        confirmButton = { TextButton({ downloads.rule(sourceId, rule, false, true); removingRule = null }) { Text(translate("Remove Rule and Delete Downloads")) } },
        dismissButton = { TextButton({ downloads.rule(sourceId, rule, false); removingRule = null }) { Text(translate("Remove Rule, Keep Downloads")) } }) }
    removeAll?.let { removing -> AlertDialog(onDismissRequest = { removeAll = null }, title = { Text(translate("Remove All Downloads")) },
        text = { Text(translate("Downloads from this source will be removed, and automatic rules will be turned off")) },
        confirmButton = { TextButton({ downloads.clear(removing.source.id); removeAll = null }) { Text(translate("Remove")) } },
        dismissButton = { TextButton({ removeAll = null }) { Text(translate("Cancel")) } }) }
}
