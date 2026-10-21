package io.github.screwys.rufin.metadata

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.MetadataEditorSnapshot
import io.github.screwys.rufin.shared.sources.ArtworkStorage
import io.github.screwys.rufin.shared.sources.MetadataFieldKind
import io.github.screwys.rufin.shared.sources.SourceMetadataException
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.DetailLinkIcon
import io.github.screwys.rufin.ui.decodePlayerArtwork
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import kotlinx.coroutines.CancellationException

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MetadataEditorScreen(model: RufinConnection, kind: String, mediaUri: String, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val connection = remember(model, kind, mediaUri) { MetadataConnection(model, context, kind, mediaUri, scope, onDismiss) }
    LaunchedEffect(connection) { connection.open() }
    DisposableEffect(connection) { onDispose { connection.close() } }
    var page by remember { mutableIntStateOf(0) }
    var confirmReload by remember { mutableStateOf(false) }
    val state = connection.state
    val editable = !connection.busy && state?.committed == null
    val import = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(connection::importArtwork) }
    Dialog(onDismissRequest = { if (!connection.busy) onDismiss() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false,
            dismissOnBackPress = !connection.busy, dismissOnClickOutside = false)) {
        BackHandler(connection.busy) { }
        Surface(Modifier.fillMaxSize()) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(translate("Edit metadata")) }, navigationIcon = {
                    IconButton(onDismiss, enabled = !connection.busy) { RufinIcon("rufin-window-close-symbolic", translate("Close")) }
                }, actions = {
                    if (state?.committed == null) TextButton(connection::save,
                        enabled = editable && state?.hasChanges == true) { Text(translate("Save")) }
                })
            }) { padding ->
                Column(Modifier.fillMaxSize().padding(padding).imePadding()) {
                    if (connection.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                    connection.failure?.let { error -> MetadataFailure(error,
                        reload = { confirmReload = true }, retry = { connection.reload() }) }
                    if (state != null) {
                        PrimaryTabRow(page) {
                            Tab(page == 0, { page = 0 }, text = { Text(translate("Metadata")) })
                            Tab(page == 1, { page = 1 }, text = { Text(translate("Artwork")) })
                        }
                        when (page) {
                            0 -> MetadataFields(connection, state, editable)
                            1 -> MetadataArtwork(connection, state, editable) { import.launch(arrayOf("image/*")) }
                        }
                    } else if (connection.failure == null) Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                }
            }
        }
    }
    (connection.failure as? SourceMetadataException.LocalAccessRequired)?.let { failure ->
        io.github.screwys.rufin.sources.LocalAccessDialog(model, failure.sourceId, failure.sourcePath,
            onDismiss = onDismiss, onSaved = { connection.reload() })
    }
    if (connection.lookupReview != null) MetadataLookupDialog(connection)
    if (confirmReload) AlertDialog(onDismissRequest = { confirmReload = false },
        title = { Text(translate("Discard changes?")) },
        text = { Text(translate("Reload metadata to review the current values.")) },
        confirmButton = { TextButton({ confirmReload = false; connection.reload() }) { Text(translate("Reload")) } },
        dismissButton = { TextButton({ confirmReload = false }) { Text(translate("Cancel")) } })

}

@Composable
private fun MetadataFields(connection: MetadataConnection, state: MetadataEditorSnapshot, editable: Boolean) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            if (state.trackCount > 1UL) Text(state.scopeSummary,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            FilledTonalButton(connection::openLookup, enabled = editable && state.externalLookupAllowed) {
                DetailLinkIcon("io.github.screwys.Rufin.external.musicbrainz", Modifier.size(24.dp))
                Spacer(Modifier.width(8.dp)); Text(translate("Search by AcoustID"))
            }
        }
        items(state.fields, key = { it.field.key }) { presented ->
            val field = presented.field
            OutlinedTextField(value = if (field.mixed) "" else field.value, onValueChange = { connection.setField(field.key, it) },
                modifier = Modifier.fillMaxWidth(), label = { Text(if (field.mixed) presented.mixedLabel else field.label) }, enabled = editable,
                readOnly = !field.writable,
                supportingText = if (presented.identified && (presented.originalMixed || presented.originalValue.isNotEmpty())) ({ Text(translate("Original: {value}").replace("{value}", if (presented.originalMixed) presented.mixedLabel else presented.originalValue)) }) else if (!field.writable) ({ Text(translate("This source cannot edit this field")) }) else null,
                trailingIcon = if (presented.identified) ({ IconButton({ connection.undo(field.key) }, enabled = editable) {
                    RufinIcon("rufin-edit-undo-symbolic", translate("Reset")) } }) else null,
                singleLine = field.kind == MetadataFieldKind.NUMBER || field.kind == MetadataFieldKind.DATE,
                keyboardOptions = KeyboardOptions(keyboardType = if (field.kind == MetadataFieldKind.NUMBER) KeyboardType.Decimal else KeyboardType.Text),
                colors = if (presented.identified) OutlinedTextFieldDefaults.colors(unfocusedBorderColor = MaterialTheme.colorScheme.primary)
                    else OutlinedTextFieldDefaults.colors(),
            )
        }
        if (state.lockWritable) item {
            SettingsGroup {
                SettingsRow(translate("Lock metadata"),
                    summary = translate("Prevent automatic metadata refreshes from replacing these values"),
                    trailing = { Switch(state.locked == true, connection::setLocked, enabled = editable) })
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MetadataArtwork(connection: MetadataConnection, state: MetadataEditorSnapshot, editable: Boolean, import: () -> Unit) {
    var artist by remember { mutableStateOf(state.searchArtist) }
    var album by remember { mutableStateOf(state.searchAlbum.orEmpty()) }
    val context = LocalContext.current
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                Artwork(connection.artwork, Modifier.size(220.dp).clip(RoundedCornerShape(12.dp)))
                if (connection.loadingArtwork) CircularProgressIndicator()
            }
            FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilledTonalButton(import, enabled = editable) {
                    RufinIcon("rufin-document-open-symbolic", null, Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(translate("Import"))
                }
                TextButton(connection::useCurrentArtwork, enabled = editable && state.hasCurrentArtwork) { Text(translate("Use current artwork")) }
                TextButton(connection::removeArtwork, enabled = editable && state.canRemoveArtwork) { Text(translate("Remove")) }
            }
            if (state.artworkStorage != ArtworkStorage.SERVER && state.canEmbedArtwork) {
                Text(translate("Artwork storage"), style = MaterialTheme.typography.titleSmall)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilterChip(state.artworkStorage == ArtworkStorage.EMBEDDED,
                        { connection.setStorage(ArtworkStorage.EMBEDDED) }, enabled = editable,
                        label = { Text(translate("Embed in track")) })
                    FilterChip(state.artworkStorage == ArtworkStorage.FOLDER,
                        { connection.setStorage(ArtworkStorage.FOLDER) }, enabled = editable,
                        label = { Text(translate("Save in folder")) })
                }
            }
        }
        if (state.externalLookupAllowed) {
            item {
                HorizontalDivider()
                Text(translate("Search artwork"), Modifier.padding(vertical = 12.dp), style = MaterialTheme.typography.titleMedium)
                OutlinedTextField(artist, { artist = it }, Modifier.fillMaxWidth(), label = { Text(translate("Artist")) }, singleLine = true, enabled = editable)
                if (state.searchAlbum != null) OutlinedTextField(album, { album = it }, Modifier.fillMaxWidth(), label = { Text(translate("Album")) }, singleLine = true, enabled = editable)
                TextButton({ connection.search(artist, album) }, enabled = editable && !connection.searching) {
                    RufinIcon("rufin-search-symbolic", null); Spacer(Modifier.width(8.dp)); Text(translate("Search"))
                }
                if (connection.searching) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (connection.results?.isEmpty() == true) Text(translate("No artwork found"))
            }
            items(connection.results.orEmpty(), key = { it.imageUrl }) { result ->
                val editor = connection.bridge
                val image by produceState<io.github.screwys.rufin.ui.PlayerArtwork?>(null, editor, result.thumbnailUrl) {
                    if (editor != null) try { value = editor.downloadArtwork(result.thumbnailUrl).use { decodePlayerArtwork(it.bytes(), 128) } }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) { }
                }
                ListItem(headlineContent = { Text(result.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    supportingContent = { Text(result.detail, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    leadingContent = { Artwork(image, Modifier.size(64.dp).clip(RoundedCornerShape(8.dp))) },
                    trailingContent = { IconButton({ context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(result.sourceUrl))) }) {
                        RufinIcon("rufin-external-link-symbolic", translate("Open"), Modifier.size(20.dp)) } },
                    modifier = Modifier.clickable(enabled = editable) { connection.chooseArtwork(result.imageUrl) })
            }
        }
    }
}

@Composable
private fun MetadataFailure(error: Throwable, reload: () -> Unit, retry: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        val message = when (error) {
            is SourceMetadataException.Conflict -> translate("Metadata changed before it was saved. Reload it before trying again.")
            is SourceMetadataException.LocalAccessRequired -> translate("Local file access is required before these tags can be saved.") + "\n" + error.sourcePath
            is SourceMetadataException.SavedRefreshFailed -> translate("Metadata was saved, but refreshing the source failed.") + "\n" + error.v1
            is SourceMetadataException.PartiallySaved -> translate("Some changes were saved. Close this editor and reopen it to review the result.") + "\n" + error.reason
            is SourceMetadataException.Unavailable -> translate("Metadata editing is no longer available")
            is SourceMetadataException.InvalidInput -> error.v1
            is SourceMetadataException.Write -> error.v1
            else -> error.message.orEmpty()
        }
        io.github.screwys.rufin.ui.ErrorNotice(message)
        if (error is SourceMetadataException.Conflict) TextButton(reload) { Text(translate("Reload")) }
        if (error is SourceMetadataException.Unavailable) TextButton(retry) { Text(translate("Retry")) }
    }
}
