package io.github.screwys.rufin.browse

import android.app.Activity
import android.content.Intent
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidLibrary
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.settingsTextFieldColors
import io.github.screwys.rufin.shared.library.PlaylistPathMode
import io.github.screwys.rufin.shared.rufin_core.PlaylistArtworkEditing
import io.github.screwys.rufin.shared.rufin_core.PlaylistFileSettings
import io.github.screwys.rufin.shared.rufin_core.SourceSummary
import io.github.screwys.rufin.shared.rufin_core.updatePlaylist
import io.github.screwys.rufin.shared.sources.ArtworkChange
import io.github.screwys.rufin.shared.sources.ImageBytes
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.decodePlayerArtwork
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream

private class PlaylistEditorState(
    private val model: RufinConnection,
    val library: AndroidLibrary,
    private val key: Long,
    initialName: String,
    private val scope: CoroutineScope,
    private val changed: () -> Unit,
) {
    var name by mutableStateOf(initialName)
    private var savedName = initialName
    var files by mutableStateOf<PlaylistFileSettings?>(null)
    var artwork by mutableStateOf<PlaylistArtworkEditing?>(null)
    var public by mutableStateOf<Boolean?>(null)
    private var savedPublic: Boolean? = null
    var autoRefresh by mutableStateOf(true)
    var autoSave by mutableStateOf<Boolean?>(null)
    var pathMode by mutableStateOf(PlaylistPathMode.AUTOMATIC)
    var fileName by mutableStateOf("")
    var busy by mutableStateOf(false)
    var failure by mutableStateOf<Throwable?>(null)
    var conflict by mutableStateOf(false)
    var preview by mutableStateOf<PlayerArtwork?>(null)
    var removed by mutableStateOf(false)
    private var image: ImageBytes? = null
    val hasImage get() = image != null || !removed && artwork?.binding != null

    suspend fun load() {
        files = library.playlistFileSettings(key)
        artwork = library.playlistArtworkEditing(key)
        savedPublic = if (files?.canLink == true) null else library.playlistPublic(key)
        public = savedPublic
        files?.link?.let { link ->
            autoRefresh = link.autoRefresh
            autoSave = link.autoSave
            pathMode = link.pathMode
            fileName = if (link.path.startsWith("content:")) files?.displayPath.orEmpty()
                else link.path.substringAfterLast('/').substringAfterLast('\\')
        }
    }

    fun action(work: suspend () -> Unit) {
        scope.launch {
            busy = true
            failure = null
            try { work() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) {
                failure = error
                conflict = library.playlistFileIsConflict(error.userMessage())
            } finally { busy = false }
        }
    }

    suspend fun save() {
        val source = model.sourceOwner ?: error(translate("The connection no longer exists"))
        val artworkChange = if (removed) ArtworkChange.Remove else image?.let { ArtworkChange.Replace(it) }
        if (files?.canLink == true) {
            library.updatePlaylistFileOptions(key, name.trim(), autoRefresh, autoSave, pathMode, artworkChange)
        } else {
            updatePlaylist(source, key, name.trim().takeIf { it != savedName }, public?.takeIf { it != savedPublic }, artworkChange)
        }
        image?.destroy()
        image = null
        removed = false
        preview = null
        savedName = name.trim()
        name = savedName
        load()
        changed()
    }

    suspend fun refresh() {
        image?.destroy()
        image = null
        removed = false
        preview = null
        name = library.playlistName(key)
        savedName = name
        load()
        changed()
    }

    suspend fun renameFile() {
        library.renamePlaylistFile(key, fileName.trim())
        files = library.playlistFileSettings(key)
        files?.link?.let { link ->
            fileName = if (link.path.startsWith("content:")) files?.displayPath.orEmpty()
                else link.path.substringAfterLast('/').substringAfterLast('\\')
        }
        changed()
    }

    suspend fun setImage(value: ImageBytes) {
        try {
            val decoded = decodePlayerArtwork(value.bytes(), 1024)
            image?.destroy()
            image = value
            preview = decoded
            removed = false
        } catch (error: Exception) {
            value.destroy()
            throw error
        }
    }

    fun removeImage() {
        image?.destroy()
        image = null
        preview = null
        removed = true
    }

    fun regenerate() = action { library.regeneratePlaylistArtwork(key)?.let { setImage(it) } }

    suspend fun fileAction(action: String, force: Boolean = false) {
        when (action) {
            "save" -> library.savePlaylistFile(key, force)
            "reload" -> library.reloadPlaylistFile(key, force)
            "unlink" -> library.unlinkPlaylistFile(key)
            "delete" -> library.deletePlaylistFile(key)
        }
        if (action != "delete") refresh() else changed()
    }

    fun close() { image?.destroy(); image = null }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
internal fun PlaylistEditor(
    model: RufinConnection, library: AndroidLibrary, player: PlayerConnection,
    playlist: Long, initialName: String, onChanged: () -> Unit, onDismiss: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val state = remember(library, playlist) { PlaylistEditorState(model, library, playlist, initialName, scope, onChanged) }
    val context = LocalContext.current
    var linkChoice by remember { mutableStateOf(false) }
    var linkDestination by remember { mutableStateOf(false) }
    var replaceLink by remember { mutableStateOf(false) }
    var sourceDestination by remember { mutableStateOf<SourceSummary?>(null) }
    var sourcePath by remember { mutableStateOf("playlist.m3u8") }
    var confirmDelete by remember { mutableStateOf(false) }
    LaunchedEffect(state) { state.action { state.load() } }
    DisposableEffect(state) { onDispose { state.close() } }

    val chooseImage = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) state.action {
            val bytes = withContext(Dispatchers.IO) {
                val limit = library.playlistArtworkLimit().toInt()
                context.contentResolver.openInputStream(uri)?.use { input ->
                    val output = ByteArrayOutputStream()
                    val buffer = ByteArray(8192)
                    var remaining = limit + 1
                    while (remaining > 0) {
                        val count = input.read(buffer, 0, minOf(buffer.size, remaining))
                        if (count < 0) break
                        output.write(buffer, 0, count)
                        remaining -= count
                    }
                    output.toByteArray()
                } ?: error(translate("Could not read artwork"))
            }
            state.setImage(library.preparePlaylistArtwork(bytes))
        }
    }
    val chooseFile = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val uri = result.data?.data
        if (result.resultCode == Activity.RESULT_OK && uri != null) state.action {
            val flags = result.data!!.flags
            val read = flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0
            val write = flags and Intent.FLAG_GRANT_WRITE_URI_PERMISSION != 0
            when {
                read && write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                read -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            }
            library.linkPlaylistFile(playlist, null, uri.toString(), replaceLink)
            state.refresh()
        }
    }
    fun chooseDevice(create: Boolean = false) {
        linkDestination = false
        val intent = Intent(if (create) Intent.ACTION_CREATE_DOCUMENT else Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = if (create) "audio/x-mpegurl" else "*/*"
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            if (create) putExtra(Intent.EXTRA_TITLE, state.name.trim().ifEmpty { "playlist" }.replace('/', '_').replace('\\', '_') + ".m3u8")
        }
        chooseFile.launch(intent)
    }
    fun chooseLink(replace: Boolean) {
        linkChoice = false
        replaceLink = replace
        state.action {
            state.save()
            val selected = model.sources?.selectedSourceId
            val editable = selected?.let { id -> withContext(Dispatchers.IO) { model.sourceOwner?.editable(id) } }
            sourceDestination = editable?.takeIf { it.fileSettings != null }?.source
            if (sourceDestination == null && !replace) chooseDevice() else linkDestination = true
        }
    }
    fun actOnFile(action: String) = state.action {
        state.save()
        state.fileAction(action)
        if (action == "delete") onDismiss()
    }

    Dialog(onDismissRequest = { if (!state.busy) onDismiss() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false,
            dismissOnBackPress = !state.busy, dismissOnClickOutside = false)) {
        BackHandler(state.busy) { }
        Surface(Modifier.fillMaxSize()) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(translate("Edit Playlist")) }, navigationIcon = {
                    IconButton(onDismiss, enabled = !state.busy) { RufinIcon("rufin-window-close-symbolic", translate("Close")) }
                }, actions = {
                    TextButton({ state.action { state.save(); onDismiss() } }, enabled = !state.busy && state.files != null && state.name.isNotBlank()) { Text(translate("Apply")) }
                })
            }) { padding ->
                Column(Modifier.fillMaxWidth().padding(padding).imePadding().verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                    state.failure?.let { ErrorNotice(it) }
                    if (state.files == null && !state.busy) TextButton({ state.action { state.load() } }) { Text(translate("Retry")) }
                    OutlinedTextField(state.name, { state.name = it }, Modifier.fillMaxWidth(), label = { Text(translate("Playlist name")) }, singleLine = true, enabled = !state.busy, colors = settingsTextFieldColors())
                    state.artwork?.let { artwork ->
                        val bindings = listOfNotNull(if (state.removed || artwork.binding == null) artwork.automaticBinding else artwork.binding)
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                            if (state.preview != null) Artwork(state.preview, Modifier.size(220.dp).clip(RoundedCornerShape(12.dp)))
                            else PlaylistCover(bindings, player)
                        }
                        if (artwork.writable) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            FilledTonalButton({ chooseImage.launch(arrayOf("image/*")) }, enabled = !state.busy) { Text(translate("Add image")) }
                            TextButton(state::regenerate, enabled = !state.busy) { Text(translate("Regenerate")) }
                            TextButton(state::removeImage, enabled = !state.busy && state.hasImage) { Text(translate("Delete")) }
                        }
                    }
                    state.public?.let { public ->
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
                            Text(translate("Public"))
                            Switch(public, { state.public = it }, enabled = !state.busy)
                        }
                    }
                    state.files?.takeIf { it.canLink }?.let { files ->
                        HorizontalDivider()
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text(translate("Linked file"), style = MaterialTheme.typography.titleMedium)
                                Text(files.displayPath ?: translate("No file linked"), style = MaterialTheme.typography.bodyMedium)
                            }
                            TextButton({ linkChoice = true }, enabled = !state.busy && state.name.isNotBlank()) { Text(translate("Open")) }
                        }
                        files.link?.let { link ->
                            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                OutlinedTextField(state.fileName, { state.fileName = it }, Modifier.weight(1f), label = { Text(translate("Filename")) }, singleLine = true, enabled = !state.busy, colors = settingsTextFieldColors())
                                TextButton({ state.action { state.renameFile() } }, enabled = !state.busy && state.fileName.isNotBlank()) { Text(translate("Apply")) }
                            }
                            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
                                Text(translate("Refresh automatically"), Modifier.weight(1f))
                                Switch(state.autoRefresh, { state.autoRefresh = it }, enabled = !state.busy)
                            }
                            PlaylistChoice(translate("Save changes automatically"), listOf(null, false, true), state.autoSave,
                                { translate(when (it) { null -> "Use source setting"; false -> "Off"; true -> "On" }) }, !state.busy) { state.autoSave = it }
                            PlaylistChoice(translate("Path format"), PlaylistPathMode.entries, state.pathMode,
                                { translate(when (it) { PlaylistPathMode.AUTOMATIC -> "Automatic"; PlaylistPathMode.RELATIVE -> "Relative"; PlaylistPathMode.ABSOLUTE -> "Absolute" }) }, !state.busy) { state.pathMode = it }
                            link.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                TextButton({ actOnFile("save") }, enabled = !state.busy && state.name.isNotBlank()) { Text(translate("Save")) }
                                TextButton({ actOnFile("reload") }, enabled = !state.busy && state.name.isNotBlank()) { Text(translate("Reload")) }
                                TextButton({ actOnFile("unlink") }, enabled = !state.busy && state.name.isNotBlank()) { Text(translate("Unlink")) }
                                TextButton({ confirmDelete = true }, enabled = !state.busy && state.name.isNotBlank(), colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text(translate("Delete")) }
                            }
                        }
                    }
                }
            }
        }
    }
    if (linkChoice) AlertDialog(onDismissRequest = { linkChoice = false }, title = { Text(translate("Link to file...")) },
        confirmButton = { TextButton({ chooseLink(false) }) { Text(translate("Load file contents")) } },
        dismissButton = { Column(horizontalAlignment = Alignment.End) {
            TextButton({ chooseLink(true) }) { Text(translate("Replace file contents")) }
            TextButton({ linkChoice = false }) { Text(translate("Cancel")) }
        } })
    if (linkDestination) AlertDialog(onDismissRequest = { linkDestination = false },
        title = { Text(sourceDestination?.name ?: translate("Link to file...")) },
        text = if (sourceDestination != null) ({ OutlinedTextField(sourcePath, { sourcePath = it }, label = { Text(translate("Filename")) }, singleLine = true) }) else null,
        confirmButton = { TextButton({
            val destination = sourceDestination
            if (destination == null) chooseDevice()
            else {
                linkDestination = false
                state.action { library.linkPlaylistFile(playlist, destination.id, sourcePath.trim(), replaceLink); state.refresh() }
            }
        }, enabled = sourceDestination == null || sourcePath.isNotBlank()) { Text(translate("Open")) } },
        dismissButton = { Column(horizontalAlignment = Alignment.End) {
            if (sourceDestination != null) TextButton({ chooseDevice() }) { Text(translate("Device")) }
            if (replaceLink) TextButton({ chooseDevice(true) }) { Text(translate("New file")) }
            TextButton({ linkDestination = false }) { Text(translate("Cancel")) }
        } })
    if (confirmDelete) AlertDialog(onDismissRequest = { confirmDelete = false }, title = { Text(translate("Delete playlist file")) },
        confirmButton = { TextButton({ confirmDelete = false; actOnFile("delete") }) { Text(translate("Delete")) } },
        dismissButton = { TextButton({ confirmDelete = false }) { Text(translate("Cancel")) } })
    if (state.conflict) AlertDialog(onDismissRequest = { state.conflict = false }, title = { Text(translate("Playlist file changed")) },
        confirmButton = { TextButton({ state.conflict = false; state.action { state.fileAction("reload", true) } }) { Text(translate("Reload from file")) } },
        dismissButton = { Column(horizontalAlignment = Alignment.End) {
            TextButton({ state.conflict = false; state.action { state.fileAction("save", true) } }) { Text(translate("Replace file contents")) }
            TextButton({ state.conflict = false }) { Text(translate("Cancel")) }
        } })
}

@Composable
private fun PlaylistCover(bindings: List<ByteArray>, player: PlayerConnection) {
    val size = 220.dp
    if (bindings.size <= 1) Artwork(rememberArtwork(bindings.firstOrNull(), player, with(LocalDensity.current) { size.roundToPx() }), Modifier.size(size).clip(RoundedCornerShape(12.dp)))
    else Column(Modifier.size(size).clip(RoundedCornerShape(12.dp))) {
        repeat(2) { y -> Row { repeat(2) { x -> Artwork(rememberArtwork(bindings[(y * 2 + x) % bindings.size], player, with(LocalDensity.current) { (size / 2).roundToPx() }), Modifier.size(size / 2)) } } }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun <T> PlaylistChoice(title: String, values: List<T>, selected: T, label: (T) -> String, enabled: Boolean, choose: (T) -> Unit) {
    Text(title, style = MaterialTheme.typography.titleSmall)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        values.forEach { value -> FilterChip(selected == value, { choose(value) }, enabled = enabled, label = { Text(label(value)) }) }
    }
}
