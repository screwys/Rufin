package io.github.screwys.rufin.metadata

import io.github.screwys.rufin.ui.PageLoading

import io.github.screwys.rufin.ui.ChoiceChip

import io.github.screwys.rufin.shared.rufin_core.MetadataItemId
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import io.github.screwys.rufin.ui.CoverShape
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
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MetadataEditorScreen(model: RufinConnection, item: MetadataItemId?,
    selection: (() -> io.github.screwys.rufin.shared.library.MemberSelection)? = null, onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val connection = remember(model, item) { MetadataConnection(model, context, item, scope, onDismiss, selection) }
    LaunchedEffect(connection) { connection.open() }
    DisposableEffect(connection) { onDispose { connection.close() } }
    var page by remember { mutableIntStateOf(0) }
    var confirmReload by remember { mutableStateOf(false) }
    val state = connection.state
    val editable = !connection.busy && state?.committed == null
    val drawer = rememberDrawerState(DrawerValue.Closed)
    LaunchedEffect(state?.selectedCount) { if ((state?.selectedCount ?: 1UL) > 1UL) page = 0 }
    val import = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(connection::importArtwork) }
    Dialog(onDismissRequest = { if (!connection.busy) onDismiss() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false,
            dismissOnBackPress = !connection.busy, dismissOnClickOutside = false)) {
        BackHandler(connection.busy) { }
        Surface(Modifier.fillMaxSize()) {
            ModalNavigationDrawer(drawerState = drawer, gesturesEnabled = connection.batch != null, drawerContent = {
                connection.batch?.let { batch -> ModalDrawerSheet {
                    MetadataTracks(batch, batch.trackCount(), batch.scopeTitle(), connection.scopeActive,
                        connection.allTracks, connection.selectedTracks, connection.rowsRevision,
                        load = { offset, limit -> connection.tracks(offset, limit) },
                        onScope = { connection.selectScope(); scope.launch { drawer.close() } },
                        onAll = { connection.selectAllTracks(); scope.launch { drawer.close() } },
                        onTrack = { index, toggle -> connection.selectTrack(index, toggle); if (!toggle) scope.launch { drawer.close() } })
                } }
            }) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(translate("Edit metadata")) }, navigationIcon = {
                    IconButton(onDismiss, enabled = !connection.busy) { RufinIcon("rufin-window-close-symbolic", translate("Close")) }
                }, actions = {
                    if (connection.batch != null) TextButton({ scope.launch { drawer.open() } }) { Text(translate("Tracks")) }
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
                            if (state.selectedCount == 1UL) Tab(page == 1, { page = 1 }, text = { Text(translate("Artwork")) })
                        }
                        when (page) {
                            0 -> MetadataFields(connection, state, editable, item is MetadataItemId.Track)
                            1 -> MetadataArtwork(connection, state, editable) { import.launch(arrayOf("image/*")) }
                        }
                    } else if (connection.failure == null) PageLoading()
                }
            }
        }
    }
    }
    (connection.failure as? SourceMetadataException.LocalAccessRequired)?.let { failure ->
        io.github.screwys.rufin.sources.LocalAccessDialog(model, failure.sourceId, failure.sourcePath,
            onDismiss = onDismiss, onSaved = { connection.reload() })
    }
    if (connection.lookupReview != null) MetadataLookupDialog(connection)
    connection.saveConfirmation?.let { summary ->
        AlertDialog(onDismissRequest = connection::cancelSave,
            title = { Text(summary.message) },
            confirmButton = { TextButton(connection::confirmSave) { Text(translate("Save")) } },
            dismissButton = { TextButton(connection::cancelSave) { Text(translate("Cancel")) } })
    }
    if (confirmReload) AlertDialog(onDismissRequest = { confirmReload = false },
        title = { Text(translate("Discard changes?")) },
        text = { Text(translate("Reload metadata to review the current values.")) },
        confirmButton = { TextButton({ confirmReload = false; connection.reload() }) { Text(translate("Reload")) } },
        dismissButton = { TextButton({ confirmReload = false }) { Text(translate("Cancel")) } })

}

@Composable
private fun MetadataFields(connection: MetadataConnection, state: MetadataEditorSnapshot, editable: Boolean, track: Boolean) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            if (state.trackCount > 1UL) Text(state.scopeSummary,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            FilledTonalButton(connection::openLookup, enabled = editable && state.externalLookupAllowed) {
                DetailLinkIcon("io.github.screwys.Rufin.external.musicbrainz", Modifier.size(24.dp))
                Spacer(Modifier.width(8.dp)); Text(if (track || connection.batch != null) translate("Search by AcoustID") else translate("Lookup"))
            }
        }
        items(state.fields, key = { it.field.key }) { presented ->
            val field = presented.field
            val number = field.kind == MetadataFieldKind.NUMBER
            val date = field.kind == MetadataFieldKind.DATE
            var picking by remember { mutableStateOf(false) }
            if (field.options.isNotEmpty()) MetadataChoiceField(connection, presented, editable)
            else if (field.kind == MetadataFieldKind.LIST) MetadataListField(connection, presented, editable)
            // The number a text starts with: tags hold `8; 8` and `120.5` too.
            else OutlinedTextField(value = if (field.mixed) "" else if (number) field.value.takeWhile { it in '0'..'9' } else field.value,
                onValueChange = { connection.setField(field.key, if (number) it.filter { digit -> digit in '0'..'9' } else it) },
                modifier = Modifier.fillMaxWidth(), label = { Text(if (field.mixed) presented.mixedLabel else field.label) }, enabled = editable,
                readOnly = !field.writable,
                supportingText = if (!field.writable) ({ Text(translate("This source cannot edit this field")) }) else null,
                // A date is typed, in whole or in part, or picked from the calendar.
                trailingIcon = if (presented.identified || date && field.writable) ({ Row {
                    if (date && field.writable) IconButton({ picking = true }, enabled = editable) {
                        RufinIcon("rufin-x-office-calendar-symbolic", translate("Choose")) }
                    MetadataReset(connection, presented, editable) } }) else null,
                singleLine = number || date,
                keyboardOptions = KeyboardOptions(keyboardType = if (number) KeyboardType.Number else KeyboardType.Text),
                colors = if (presented.identified) OutlinedTextFieldDefaults.colors(unfocusedBorderColor = MaterialTheme.colorScheme.primary)
                    else OutlinedTextFieldDefaults.colors(),
            )
            if (picking) MetadataDatePicker(field.value, { picking = false }) { connection.setField(field.key, it) }
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

@Composable
private fun MetadataReset(connection: MetadataConnection, presented: io.github.screwys.rufin.shared.rufin_core.MetadataFieldPresentation, editable: Boolean) {
    if (presented.identified) IconButton({ connection.undo(presented.field.key) }, enabled = editable) {
        RufinIcon("rufin-edit-undo-symbolic", translate("Reset")) }
}

/** A list's values as chips, each removed with a tap, under the field that adds one. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MetadataListField(connection: MetadataConnection, presented: io.github.screwys.rufin.shared.rufin_core.MetadataFieldPresentation, editable: Boolean) {
    val field = presented.field
    fun values(text: String) = text.split(';').map { it.trim() }.filter { it.isNotEmpty() }
    val all = if (field.mixed) emptyList() else values(field.value)
    // The text being typed is the list's last value already, so a save never drops it.
    var typed by remember { mutableStateOf("") }
    val chips = if (typed.isNotBlank() && all.lastOrNull() == typed.trim()) all.dropLast(1) else all
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        OutlinedTextField(typed, { text ->
                connection.setField(field.key, (chips + values(text)).joinToString("; "))
                typed = text.substringAfterLast(';').trimStart()
            }, Modifier.fillMaxWidth(), label = { Text(if (field.mixed) presented.mixedLabel else field.label) }, enabled = editable,
            readOnly = !field.writable,
            supportingText = if (!field.writable) ({ Text(translate("This source cannot edit this field")) }) else null,
            trailingIcon = if (presented.identified) ({ MetadataReset(connection, presented, editable) }) else null,
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), keyboardActions = KeyboardActions(onDone = { typed = "" }),
            colors = if (presented.identified) OutlinedTextFieldDefaults.colors(unfocusedBorderColor = MaterialTheme.colorScheme.primary)
                else OutlinedTextFieldDefaults.colors())
        if (chips.isNotEmpty()) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            chips.forEachIndexed { index, chip ->
                InputChip(false, { connection.setField(field.key, (chips.filterIndexed { other, _ -> other != index } + values(typed)).joinToString("; ")) },
                    label = { Text(chip) }, enabled = editable && field.writable,
                    trailingIcon = if (field.writable) ({ RufinIcon("rufin-window-close-symbolic", translate("Remove"), Modifier.size(InputChipDefaults.IconSize)) }) else null)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MetadataDatePicker(value: String, onDismiss: () -> Unit, onDate: (String) -> Unit) {
    fun utc() = java.util.Calendar.getInstance(java.util.TimeZone.getTimeZone("UTC"))
    // A year or a month opens on its first day; an empty field, on today.
    val parts = value.split(Regex("\\D+")).map { it.toIntOrNull() }
    val initial = parts[0]?.takeIf { it in DatePickerDefaults.YearRange }?.let { year ->
        utc().apply { clear(); set(year, (parts.getOrNull(1) ?: 1) - 1, parts.getOrNull(2) ?: 1) }.timeInMillis
    }
    val picker = rememberDatePickerState(initial)
    DatePickerDialog(onDismissRequest = onDismiss,
        confirmButton = { TextButton({
            picker.selectedDateMillis?.let { millis ->
                val date = utc().apply { timeInMillis = millis }
                onDate(String.format(java.util.Locale.ROOT, "%04d-%02d-%02d", date[java.util.Calendar.YEAR],
                    date[java.util.Calendar.MONTH] + 1, date[java.util.Calendar.DAY_OF_MONTH]))
            }
            onDismiss()
        }) { Text(translate("Choose")) } },
        dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } }) { DatePicker(picker) }
}

/** A field with a fixed vocabulary: its value is picked from a menu, one of them or, for a list, any number. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MetadataChoiceField(connection: MetadataConnection, presented: io.github.screwys.rufin.shared.rufin_core.MetadataFieldPresentation, editable: Boolean) {
    val field = presented.field
    val several = field.kind == MetadataFieldKind.LIST
    val chosen = field.value.split(';').map { it.trim() }.filter { it.isNotEmpty() }
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded, { expanded = it && editable && field.writable }) {
        OutlinedTextField(if (field.mixed) "" else field.value, {},
            Modifier.menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
            readOnly = true, singleLine = true, enabled = editable,
            label = { Text(if (field.mixed) presented.mixedLabel else field.label) },
            supportingText = if (!field.writable) ({ Text(translate("This source cannot edit this field")) }) else null,
            trailingIcon = if (presented.identified) ({ IconButton({ connection.undo(field.key) }, enabled = editable) {
                RufinIcon("rufin-edit-undo-symbolic", translate("Reset")) } })
                else ({ RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp)) }),
            colors = if (presented.identified) OutlinedTextFieldDefaults.colors(unfocusedBorderColor = MaterialTheme.colorScheme.primary)
                else OutlinedTextFieldDefaults.colors())
        ExposedDropdownMenu(expanded, { expanded = false }) {
            if (!several) DropdownMenuItem({ Text(translate("None")) }, { connection.setField(field.key, ""); expanded = false },
                leadingIcon = { if (chosen.isEmpty()) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp)) else Spacer(Modifier.size(18.dp)) })
            field.options.forEach { option ->
                val selected = option in chosen
                DropdownMenuItem({ Text(option) }, {
                    if (several) connection.setField(field.key, field.options.filter { (it in chosen) != (it == option) }.joinToString("; "))
                    else { connection.setField(field.key, option); expanded = false }
                }, leadingIcon = { if (selected) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp)) else Spacer(Modifier.size(18.dp)) })
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MetadataArtwork(connection: MetadataConnection, state: MetadataEditorSnapshot, editable: Boolean, import: () -> Unit) {
    var artist by remember(state.searchArtist) { mutableStateOf(state.searchArtist) }
    var album by remember(state.searchAlbum) { mutableStateOf(state.searchAlbum.orEmpty()) }
    val context = LocalContext.current
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                Artwork(io.github.screwys.rufin.ui.Cover(connection.artwork, !connection.loadingArtwork && connection.artwork == null), Modifier.size(220.dp).clip(CoverShape))
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
                    ChoiceChip(state.artworkStorage == ArtworkStorage.EMBEDDED,
                        { connection.setStorage(ArtworkStorage.EMBEDDED) }, enabled = editable,
                        label = { Text(translate("Embed in track")) })
                    ChoiceChip(state.artworkStorage == ArtworkStorage.FOLDER,
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
                    leadingContent = { Artwork(io.github.screwys.rufin.ui.Cover(image), Modifier.size(64.dp).clip(CoverShape)) },
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
