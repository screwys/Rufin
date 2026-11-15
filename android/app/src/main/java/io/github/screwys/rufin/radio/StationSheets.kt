package io.github.screwys.rufin.radio

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.PictureEdit
import io.github.screwys.rufin.browse.PictureEditor
import io.github.screwys.rufin.browse.rememberPictureChooser
import io.github.screwys.rufin.core.AndroidRadio
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.settingsTextFieldColors
import io.github.screwys.rufin.shared.rufin_core.PlaylistArtworkEditing
import io.github.screwys.rufin.shared.rufin_core.Station
import io.github.screwys.rufin.shared.rufin_core.StationFound
import io.github.screwys.rufin.shared.rufin_core.StationInput
import io.github.screwys.rufin.shared.rufin_core.StationProbe
import io.github.screwys.rufin.sources.SourceTextField
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SearchField
import io.github.screwys.rufin.ui.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** What Add Station shows under its field: the stations a name found, what a stream address says of
 *  itself, or why neither came. */
private sealed interface Found {
    data class Stations(val stations: List<StationFound>) : Found
    data class Stream(val probe: StationProbe) : Found
    data class Failed(val message: String) : Found
}

/** A small line under a sheet's content. */
@Composable
private fun SheetNote(text: String) {
    Text(text, Modifier.padding(horizontal = 4.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

/** Add Station: a name searches radio-browser.info as it is typed, and a stream address is read for what
 *  the stream says of itself. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AddStationSheet(model: RufinConnection, player: PlayerConnection, radio: AndroidRadio, savedOn: String?, onDismiss: () -> Unit) {
    var text by remember { mutableStateOf("") }
    var found by remember { mutableStateOf<Found?>(null) }
    var reading by remember { mutableStateOf(false) }
    // The streams added from this sheet, which core's results listed as not added.
    val added = remember { mutableStateListOf<String>() }
    val focus = remember { FocusRequester() }
    val query = text.trim()
    val address = query.startsWith("http://", ignoreCase = true) || query.startsWith("https://", ignoreCase = true)
    LaunchedEffect(Unit) { focus.requestFocus() }
    LaunchedEffect(query) {
        if (query.isEmpty()) { found = null; reading = false; return@LaunchedEffect }
        reading = true
        try {
            delay(350)
            found = try { if (address) Found.Stream(radio.probe(query)) else Found.Stations(radio.search(query)) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { Found.Failed(error.userMessage()) }
        } finally { reading = false }
    }
    // Fully open and of one height, so the results keep their place while the search reads again and
    // stay above the keyboard.
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.9f).imePadding().padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Text(translate("Add Station"), Modifier.padding(horizontal = 4.dp), style = MaterialTheme.typography.titleLarge)
            SearchField(text, { text = it }, Modifier.fillMaxWidth().focusRequester(focus), translate("Name or stream address"),
                KeyboardOptions(autoCorrectEnabled = false, keyboardType = if (address) KeyboardType.Uri else KeyboardType.Text))
            if (reading) LinearProgressIndicator(Modifier.fillMaxWidth())
            Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(rememberScrollState()).padding(bottom = 24.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp)) {
                when (val shown = found) {
                    is Found.Stations -> {
                        if (shown.stations.isEmpty()) Text(translate("No results"), Modifier.padding(horizontal = 4.dp),
                            color = MaterialTheme.colorScheme.onSurfaceVariant)
                        else SettingsGroup {
                            shown.stations.forEach { station ->
                                FoundRow(station, player, station.added || station.streamUrl in added) {
                                    model.runAction {
                                        radio.add(StationInput(station.name, station.streamUrl, station.homepage, station.logo))
                                        added += station.streamUrl
                                    }
                                }
                            }
                        }
                        SheetNote(translate("Results from radio-browser.info."))
                    }
                    is Found.Stream -> key(shown.probe) {
                        var name by remember { mutableStateOf(shown.probe.name) }
                        var homepage by remember { mutableStateOf("") }
                        SettingsGroup {
                            StationRow(shown.probe.name, listOf(shown.probe.format, shown.probe.host).filter(String::isNotBlank).joinToString(" · "),
                                null, player)
                        }
                        SourceTextField("Name", name, { name = it })
                        SourceTextField("Homepage", homepage, { homepage = it })
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                            Button({ model.runAction {
                                radio.add(StationInput(name.trim(), shown.probe.streamUrl, homepage.trim(), ""))
                                onDismiss()
                            } }, Modifier.height(44.dp), enabled = name.isNotBlank() && model.pendingActions == 0) { Text(translate("Add")) }
                        }
                    }
                    is Found.Failed -> ErrorNotice(shown.message, Modifier.padding(horizontal = 4.dp))
                    null -> {}
                }
                savedOn?.let { SheetNote(translate("Stations are saved on {saved_on}.").replace("{saved_on}", it)) }
            }
        }
    }
}

/** A row of the sheet's group: a station's picture, its name and a detail line, and what ends it. */
@Composable
private fun StationRow(name: String, detail: String, binding: ByteArray?, player: PlayerConnection, trailing: (@Composable () -> Unit)? = null) {
    ListItem(headlineContent = { Text(name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = if (detail.isBlank()) null else ({ Text(detail, maxLines = 1, overflow = TextOverflow.Ellipsis) }),
        leadingContent = { StationLogo(binding, player, 40.dp) }, trailingContent = trailing,
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        modifier = Modifier.clip(RoundedCornerShape(5.dp)))
}

/** A station a search found, ending with Add, or with Added once it is. */
@Composable
private fun FoundRow(station: StationFound, player: PlayerConnection, added: Boolean, onAdd: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    StationRow(station.name, station.detail, station.artworkBinding, player) {
        if (added) Surface(shape = RoundedCornerShape(18.dp), color = scheme.primaryContainer, contentColor = scheme.onPrimaryContainer) {
            Row(Modifier.height(36.dp).padding(start = 12.dp, end = 16.dp), verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                RufinIcon("rufin-object-select-symbolic", null, Modifier.size(14.dp))
                Text(translate("Added"), style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.Bold)
            }
        } else Button(onAdd, Modifier.height(36.dp), contentPadding = PaddingValues(horizontal = 16.dp)) {
            Text(translate("Add"), fontWeight = FontWeight.Bold)
        }
    }
}

/** Edit Station, in the playlist editor's shape: its name, its picture, its homepage, the stream of one
 *  added by its address, and, for a station with several channels, the one a tap on its card plays. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun EditStationDialog(model: RufinConnection, player: PlayerConnection, radio: AndroidRadio, station: Station, onDismiss: () -> Unit) {
    val key = station.key ?: return
    val library = model.browse.library ?: return
    var name by remember { mutableStateOf(station.name) }
    var homepage by remember { mutableStateOf(station.homepage) }
    var stream by remember { mutableStateOf(station.streamUrl) }
    var channel by remember { mutableStateOf(station.channels.firstOrNull { it.own }?.name) }
    // The card's picture shows until the station's two are read: the one chosen for it, and the one under it.
    val picture = remember { PictureEdit().also {
        it.editing = PlaylistArtworkEditing(station.artworkBinding.takeIf { station.pictureChosen },
            station.artworkBinding.takeUnless { station.pictureChosen }, true)
    } }
    var busy by remember { mutableStateOf(false) }
    var failure by remember { mutableStateOf<Throwable?>(null) }
    val scope = rememberCoroutineScope()
    fun act(work: suspend () -> Unit) {
        scope.launch {
            busy = true
            failure = null
            try { work() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error }
            finally { busy = false }
        }
    }
    LaunchedEffect(key) { act { picture.editing = radio.pictureEditing(key) } }
    DisposableEffect(picture) { onDispose { picture.clear() } }
    val chooseImage = rememberPictureChooser(library, picture, ::act)
    Dialog(onDismissRequest = { if (!busy) onDismiss() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false,
            dismissOnBackPress = !busy, dismissOnClickOutside = false)) {
        BackHandler(busy) { }
        Surface(Modifier.fillMaxSize()) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(translate("Edit Station")) }, navigationIcon = {
                    IconButton(onDismiss, enabled = !busy) { RufinIcon("rufin-window-close-symbolic", translate("Close")) }
                }, actions = {
                    Button({ act {
                        radio.edit(key, StationInput(name.trim(), stream.trim(), homepage.trim(), ""), channel, picture.change)
                        picture.clear()
                        onDismiss()
                    } }, Modifier.padding(end = 8.dp).height(40.dp), enabled = !busy && name.isNotBlank()) {
                        Text(translate("Apply"), fontWeight = FontWeight.Bold)
                    }
                })
            }) { padding ->
                Column(Modifier.fillMaxWidth().padding(padding).imePadding().verticalScroll(rememberScrollState()).padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                    failure?.let { ErrorNotice(it) }
                    val field: @Composable (String, String, (String) -> Unit) -> Unit = { label, value, change ->
                        OutlinedTextField(value, change, Modifier.fillMaxWidth(), label = { Text(translate(label)) }, singleLine = true,
                            enabled = !busy, colors = settingsTextFieldColors())
                    }
                    field("Name", name) { name = it }
                    PictureEditor(picture, !busy, chooseImage, cover = { StationLogo(it, player, 220.dp) }) {
                        FilledTonalButton(picture::remove, enabled = !busy && picture.hasImage) { Text(translate("Remove")) }
                    }
                    field("Homepage", homepage) { homepage = it }
                    // A station of Rufin's list keeps the list's streams.
                    if (station.listed == null) field("Stream address", stream) { stream = it }
                    if (station.channels.isNotEmpty()) {
                        var choosing by remember { mutableStateOf(false) }
                        ExposedDropdownMenuBox(choosing, { choosing = it && !busy }) {
                            OutlinedTextField(channel.orEmpty(), {}, Modifier.menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
                                readOnly = true, singleLine = true, enabled = !busy, label = { Text(translate("Default channel")) },
                                trailingIcon = { RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp)) }, colors = settingsTextFieldColors())
                            ExposedDropdownMenu(choosing, { choosing = false }) {
                                station.channels.forEach { option ->
                                    DropdownMenuItem({ Text(option.name) }, { channel = option.name; choosing = false },
                                        leadingIcon = {
                                            if (option.name == channel) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp))
                                            else Spacer(Modifier.size(18.dp))
                                        })
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
