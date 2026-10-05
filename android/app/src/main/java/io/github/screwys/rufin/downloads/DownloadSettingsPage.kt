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
import io.github.screwys.rufin.core.AndroidDownloadRule
import io.github.screwys.rufin.core.AndroidDownloadSource
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
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    val source = sources.find { it.id == selected } ?: sources.find { it.id == model.libraryState?.sourceId } ?: sources.firstOrNull()
    var removingRule by remember { mutableStateOf<Pair<String, AndroidDownloadRule>?>(null) }
    var removeAll by remember { mutableStateOf<AndroidDownloadSource?>(null) }
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
        if (sources.isEmpty()) Text(translate("Add a server to configure downloads."))
        else if (source != null) {
            SettingsGroup {
                PreferenceChoice(translate("Source"), source.id, sources.map { it.id to it.name }, select = { selected = it })
            }
            SettingsGroup(translate("Storage")) {
            SettingsRow(translate("Download Folder"), summary = source.directory ?: translate("Rufin data folder"), trailing = {
                TextButton({ folderSource = source.id; folder.launch(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)) }) { Text(translate("Choose")) }
            })
            if (source.directory != null) TextButton({ downloads.directory(source.id, null) }) { Text(translate("Use Rufin data folder")) }
            val qualities = listOf(null, 320u, 256u, 192u, 128u).filter { it == null || source.bitrateLimit == null || it <= source.bitrateLimit!! }
            PreferenceChoice(translate("Download quality"), source.bitrate?.toString() ?: "original",
                qualities.map { (it?.toString() ?: "original") to (if (it == null) translate("Original") else "$it kbps") },
                select = { downloads.quality(source.id, it.toUIntOrNull()) })
            }
            SettingsGroup {
            source.rules.forEach { rule -> SettingsRow(rule.title, trailing = {
                Switch(rule.enabled, { enabled -> if (enabled) downloads.rule(source.id, rule.id, true) else removingRule = source.id to rule })
            }) }
            }
            SettingsGroup(translate("Downloaded")) {
            SettingsRow(translate("Remove All Downloads"),
                summary = trackCountText(downloads.state?.queues?.find { it.sourceId == source.id }?.downloadedTracks ?: 0UL),
                trailing = { TextButton({ removeAll = source }) { Text(translate("Remove")) } })
            }
        }
    }
    removingRule?.let { (sourceId, rule) -> AlertDialog(onDismissRequest = { removingRule = null }, title = { Text(rule.title) },
        text = { Text(translate("Choose whether to keep the completed downloads.")) },
        confirmButton = { TextButton({ downloads.rule(sourceId, rule.id, false, true); removingRule = null }) { Text(translate("Remove Rule and Delete Downloads")) } },
        dismissButton = { TextButton({ downloads.rule(sourceId, rule.id, false); removingRule = null }) { Text(translate("Remove Rule, Keep Downloads")) } }) }
    removeAll?.let { removing -> AlertDialog(onDismissRequest = { removeAll = null }, title = { Text(translate("Remove All Downloads")) },
        text = { Text(translate("Downloads from this server will be removed, and automatic rules will be turned off")) },
        confirmButton = { TextButton({ downloads.clear(removing.id); removeAll = null }) { Text(translate("Remove")) } },
        dismissButton = { TextButton({ removeAll = null }) { Text(translate("Cancel")) } }) }
}
