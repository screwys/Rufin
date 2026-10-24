package io.github.screwys.rufin.settings

import android.text.format.Formatter
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.cacheEntryCountText
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.rufin_core.CacheKind
import io.github.screwys.rufin.shared.rufin_core.StorageSummary
import io.github.screwys.rufin.shared.rufin_core.StorageReset
import io.github.screwys.rufin.shared.rufin_core.SourceOperation
import io.github.screwys.rufin.shared.rufin_core.SourceProgressStage
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun StorageSettingsPage(model: RufinConnection, preferences: PreferencesConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var summary by remember { mutableStateOf<StorageSummary?>(null) }
    var busy by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<String?>(null) }
    var confirm by remember { mutableStateOf<StorageReset?>(null) }
    var cacheToClear by remember { mutableStateOf<CacheKind?>(null) }
    var resetting by remember { mutableStateOf(false) }
    suspend fun refresh(clear: CacheKind? = null) {
        busy = true
        error = null
        try {
            model.connectedService().runtime.value!!.getOrThrow().storage().use { storage ->
                val next = storage.summary(clear)
                summary = next
                error = next.errors.takeIf { it.isNotEmpty() }?.joinToString("\n")
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { error = failure.userMessage() }
        finally { busy = false }
    }
    LaunchedEffect(model.service, model.ready) { if (model.ready) refresh() }
    // The downloader reports its own count; the summary is measured again only once it ends.
    val download = model.sourceOperation as? SourceOperation.Downloading
    val downloadKind = when (download?.progress?.stage) {
        SourceProgressStage.ARTWORK -> CacheKind.ARTWORK
        SourceProgressStage.LYRICS -> CacheKind.LYRICS
        else -> null
    }
    LaunchedEffect(download != null) { if (download == null && summary != null) refresh() }
    SettingsPage {
        Surface(color = MaterialTheme.colorScheme.secondaryContainer, shape = MaterialTheme.shapes.large) {
            Row(Modifier.fillMaxWidth().padding(20.dp), verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                RufinIcon("rufin-drive-harddisk-symbolic", null, Modifier.size(32.dp))
                Column(Modifier.weight(1f)) {
                    Text(translate("Cached data"), style = MaterialTheme.typography.labelLarge)
                    Text(summary?.totalBytes?.let { Formatter.formatFileSize(context, it.toLong()) }
                        ?: if (busy) "..." else translate("Unavailable"), style = MaterialTheme.typography.headlineMedium)
                }
                if (busy) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
            }
        }
        preferences.applicationSettings?.let { settings ->
            val modes = listOf("false" to translate("Download Lazily"), "true" to translate("Download Automatically"))
            SettingsGroup(translate("Cache")) {
                PreferenceChoice(translate("Artwork"), settings.downloadArtworkAutomatically.toString(), modes,
                    enabled = { model.ready && !resetting },
                    select = { preferences.changePreference { setDownloadArtworkAutomatically(it.toBoolean()) } })
                PreferenceChoice(translate("Lyrics"), settings.downloadLyricsAutomatically.toString(), modes,
                    enabled = { model.ready && !resetting },
                    select = { preferences.changePreference { setDownloadLyricsAutomatically(it.toBoolean()) } })
            }
        }
        SettingsGroup {
            CacheKind.entries.forEach { kind ->
                val usage = summary?.caches?.find { it.kind == kind }?.usage
                ListItem(headlineContent = {
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(translate(kind.title()), Modifier.alignByBaseline())
                        if (kind != CacheKind.JAPANESE_DICTIONARY) {
                            Text(model.sourceStatus?.takeIf { kind == downloadKind }
                                ?: usage?.let { cacheEntryCountText(it.entries) } ?: if (busy) "..." else translate("Unavailable"),
                                modifier = Modifier.alignByBaseline(), style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }, trailingContent = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(usage?.let { Formatter.formatFileSize(context, it.bytes.toLong()) } ?: if (busy) "..." else translate("Unavailable"),
                            style = MaterialTheme.typography.labelMedium)
                        IconButton({ cacheToClear = kind }, enabled = !busy && model.ready && (usage?.entries ?: 0UL) > 0UL) {
                            RufinIcon("rufin-user-trash-symbolic", translate(kind.clearTitle()))
                        }
                    }
                }, colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainer))
            }
        }
        error?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton({ confirm = StorageReset.KEEP_CREDENTIALS }, enabled = model.ready && !busy,
                modifier = Modifier.weight(1f), colors = settingsButtonColors()) { Text(translate("Clear all cache")) }
            Button({ confirm = StorageReset.ALL_DATA }, enabled = model.ready && !busy,
                modifier = Modifier.weight(1f), colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.errorContainer, contentColor = MaterialTheme.colorScheme.onErrorContainer)) {
                Text(translate("Clear all data"))
            }
        }
    }
    confirm?.let { kind ->
        AlertDialog(onDismissRequest = { confirm = null }, title = {
            Text(translate(if (kind == StorageReset.ALL_DATA) "Clear all data?" else "Clear all cache?"))
        }, text = { Text(translate(if (kind == StorageReset.ALL_DATA)
            "Remove all Rufin data, settings and saved logins? Rufin will restart after clearing the data."
            else "Remove all Rufin data and settings except saved connections and credentials? Rufin will restart after clearing the data.")) },
            confirmButton = { TextButton({
                confirm = null
                resetting = true
                scope.launch {
                    try { model.resetStorage(kind) }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (failure: Exception) { error = failure.userMessage(); resetting = false }
                }
            }) { Text(translate("Clear")) } },
            dismissButton = { TextButton({ confirm = null }) { Text(translate("Cancel")) } })
    }
    cacheToClear?.let { kind ->
        AlertDialog(onDismissRequest = { cacheToClear = null }, title = {
            Text(translate("Clear this cache?"))
        }, confirmButton = {
            TextButton({
                cacheToClear = null
                scope.launch { refresh(kind) }
            }) { Text(translate("Clear")) }
        }, dismissButton = {
            TextButton({ cacheToClear = null }) { Text(translate("Cancel")) }
        })
    }
    if (resetting) Dialog(onDismissRequest = {}, properties = DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false)) {
        Surface(shape = MaterialTheme.shapes.large) { CircularProgressIndicator(Modifier.padding(32.dp)) }
    }
}

private fun CacheKind.title() = when (this) {
    CacheKind.WAVEFORMS -> "Waveforms"
    CacheKind.LOUDNESS -> "EBU R128 analysis"
    CacheKind.FINGERPRINTS -> "Audio fingerprints"
    CacheKind.METADATA_LOOKUPS -> "Metadata lookups"
    CacheKind.ARTWORK -> "Artwork"
    CacheKind.LYRICS -> "Lyrics"
    CacheKind.JAPANESE_DICTIONARY -> "Japanese dictionary"
}

private fun CacheKind.clearTitle() = when (this) {
    CacheKind.WAVEFORMS -> "Clear waveform cache"
    CacheKind.LOUDNESS -> "Clear EBU R128 cache"
    CacheKind.FINGERPRINTS -> "Clear fingerprint cache"
    CacheKind.METADATA_LOOKUPS -> "Clear lookup cache"
    CacheKind.ARTWORK -> "Clear artwork cache"
    CacheKind.LYRICS -> "Clear lyrics cache"
    CacheKind.JAPANESE_DICTIONARY -> "Clear Japanese dictionary"
}
