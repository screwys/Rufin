package io.github.screwys.rufin.player

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.SystemClock
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.drawText
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsExpander
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.SettingsSwitch
import io.github.screwys.rufin.settings.settingsTextFieldColors
import io.github.screwys.rufin.shared.audio_processing.*
import io.github.screwys.rufin.shared.playback.PlaybackOutput
import io.github.screwys.rufin.shared.rufin_core.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.util.Locale
import kotlin.math.*

private enum class PresetNameAction { Save, Duplicate, Rename }
internal fun presetDisplayName(name: String, modified: Boolean): String = if (modified) "$name *" else name

private enum class PresetTransfer { Import, Export }

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun EqualizerSettings(player: PlayerConnection, active: Boolean) {
    val owner = player.playbackOwner ?: return
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val subscription = remember(owner) { owner.subscribeEqualizer("") }
    var view by remember(owner) { mutableStateOf(owner.equalizerView("")) }
    var presets by remember(owner) { mutableStateOf(owner.equalizerPresets()) }
    var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) }
    var response by remember { mutableStateOf(true) }
    var spectrum by remember { mutableStateOf(false) }
    var parametric by remember { mutableStateOf(false) }
    var outputOpen by remember { mutableStateOf(false) }
    var transfer by rememberSaveable { mutableStateOf<PresetTransfer?>(null) }
    var transferOutput by rememberSaveable { mutableStateOf("") }
    var transferFormat by rememberSaveable { mutableStateOf(EqPresetFormat.JSON) }
    var transferText by rememberSaveable { mutableStateOf("") }
    var transferError by remember { mutableStateOf<String?>(null) }
    var nameAction by rememberSaveable { mutableStateOf<PresetNameAction?>(null) }
    var name by rememberSaveable { mutableStateOf("") }
    val remote = (player.playback?.controls?.playbackOutput as? PlaybackOutput.Remote)?.v1
    val routes = player.service?.outputs?.routes?.collectAsState()?.value.orEmpty()
    val receiverProcessing = !view.available
    val output = view.output
    val profile = view.profile
    LaunchedEffect(profile.layout) { parametric = profile.layout == BandLayout.PARAMETRIC }
    val editingParametric = parametric
    val frequencies = remember(profile.bands) {
        val low = ln(min(20.0, profile.bands.minOfOrNull { it.frequencyHz } ?: 20.0))
        val high = ln(max(20000.0, profile.bands.maxOfOrNull { it.frequencyHz } ?: 20000.0))
        List(256) { exp(low + (high - low) * it / 255.0) }
    }
    var curve by remember { mutableStateOf<List<Double>>(emptyList()) }
    var clippingUntil by remember { mutableLongStateOf(0L) }

    LaunchedEffect(subscription) {
        try { while (isActive) {
            view = subscription.next()
            if (view.peakDb.any { it > 0.0 }) clippingUntil = SystemClock.elapsedRealtime() + 1000L
        } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
        finally { subscription.destroy() }
    }
    LaunchedEffect(clippingUntil) {
        if (clippingUntil > 0L) {
            delay((clippingUntil - SystemClock.elapsedRealtime()).coerceAtLeast(0L))
            clippingUntil = 0L
        }
    }
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ -> foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(subscription, active, foreground, spectrum, view.cast, receiverProcessing) {
        val visible = active && foreground && !view.cast && !receiverProcessing
        subscription.setMeasurement(visible, visible && spectrum)
    }
    LaunchedEffect(owner, output, profile, view.sampleRate, view.enabled, response, active, foreground, frequencies) {
        curve = if (response && active && foreground) withContext(Dispatchers.Default) { owner.equalizerResponse(output, frequencies) } else emptyList()
    }
    fun operation(block: suspend () -> Unit) = player.model.runAction {
        block()
        presets = owner.equalizerPresets()
    }
    fun preview(edit: EqEdit) {
        try { owner.previewEqualizer(output, edit) }
        catch (error: Exception) { player.reportError(error) }
    }
    fun commit() = operation { owner.commitEqualizer(output) }
    fun change(edit: EqEdit) { preview(edit); commit() }
    fun openTransfer(kind: PresetTransfer) {
        transferOutput = output
        transferFormat = EqPresetFormat.JSON
        transferError = null
        transfer = kind
        transferText = ""
    }
    LaunchedEffect(transfer, transferFormat) {
        if (transfer == PresetTransfer.Export) {
            try {
                transferText = owner.exportEqualizer(transferOutput, transferFormat)
                transferError = null
            } catch (error: Exception) {
                transferText = ""
                transferError = error.message
            }
        }
    }
    val displayName = if (view.presetBuiltin || view.presetId == null && view.saveTarget == null) translate(view.name) else view.name
    fun savePreset() {
        if (view.saveTarget != null) operation { owner.saveEqualizerPreset(output, null) }
        else { name = displayName; nameAction = PresetNameAction.Save }
    }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SettingsGroup {
            SettingsRow(translate("Equalizer"), trailing = { Switch(view.enabled, null, enabled = !receiverProcessing) },
                modifier = Modifier.toggleable(view.enabled, enabled = !receiverProcessing, role = Role.Switch) { enabled ->
                    preview(EqEdit.Enabled(enabled))
                    operation {
                        if (view.cast) owner.applyEqualizer(output)
                        owner.commitEqualizer(output)
                    }
                })
        }
        if (!receiverProcessing) {
            Column(Modifier.fillMaxWidth()) {
                PresetActions(
                    saveEnabled = view.modified || view.presetId == null,
                    editEnabled = !view.presetBuiltin && (view.presetId ?: view.saveTarget) != null,
                    save = ::savePreset,
                    duplicate = { name = displayName; nameAction = PresetNameAction.Duplicate },
                    rename = { name = displayName; nameAction = PresetNameAction.Rename },
                    importPreset = { openTransfer(PresetTransfer.Import) },
                    exportPreset = { openTransfer(PresetTransfer.Export) },
                    delete = { (view.presetId ?: view.saveTarget)?.let { operation { owner.deleteEqualizerPreset(it) } } },
                )
                Box(Modifier.fillMaxWidth()) {
                    var picker by remember { mutableStateOf(false) }
                    FilledTonalButton({ picker = true }, Modifier.fillMaxWidth(), colors = equalizerButtonColors()) {
                        Text(presetDisplayName(displayName, view.modified), Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(18.dp))
                    }
                    DropdownMenu(picker, { picker = false }, Modifier.heightIn(max = 400.dp)) {
                        DropdownMenuItem(text = { Text(translate("Custom")) }, onClick = { picker = false; operation { owner.selectEqualizerPreset(output, null) } })
                        presets.forEach { preset -> DropdownMenuItem(
                            text = { Text(if (preset.builtin) translate(preset.name) else preset.name) },
                            onClick = { picker = false; operation { owner.selectEqualizerPreset(output, preset.id) } }) }

                    }
                }
                BoxWithConstraints(Modifier.fillMaxWidth()) {
                    val deviceWidth = if (maxWidth < 360.dp) 132.dp else 160.dp
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Row(Modifier.weight(1f).toggleable(view.assigned, role = Role.Checkbox) { assigned -> operation { owner.assignEqualizer(output, assigned) } },
                            verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(view.assigned, null, modifier = Modifier.size(40.dp))
                            Text(translate("Use for this output"), Modifier.weight(1f), maxLines = 2,
                                style = MaterialTheme.typography.bodySmall, overflow = TextOverflow.Ellipsis)
                        }
                        FilledTonalButton({ outputOpen = true }, Modifier.width(deviceWidth), colors = equalizerButtonColors()) {
                            Text(remote?.name ?: routes.firstOrNull { it.selected }?.name ?: translate("This device"),
                                Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(18.dp))
                        }
                        FilledTonalIconToggleButton(checked = editingParametric, onCheckedChange = { parametric = it },
                            modifier = Modifier.size(48.dp).semantics { stateDescription = translate(if (editingParametric) "Parametric" else "Graphic") }) {
                            io.github.screwys.rufin.ui.RufinIcon(if (editingParametric) "rufin-sliders-vertical-symbolic" else "rufin-spring-graph-symbolic",
                                translate(if (editingParametric) "Graphic" else "Parametric"), Modifier.size(24.dp))
                        }
                    }
                }
            }
            EqualizerEditor(view, editingParametric, frequencies, curve, response, spectrum && !view.cast,
                onResponse = { response = !response }, onSpectrum = { spectrum = !spectrum },
                preview = ::preview, commit = ::commit)
            SettingsGroup {
                SettingsSwitch(translate("Automatic preamp"), profile.automaticPreamp) { change(EqEdit.AutomaticPreamp(it)) }
                if (profile.automaticPreamp) SettingsRow(translate("Preamp"), trailing = {
                    Text(if (view.sampleRate == null) "-" else "${number(view.preampDb)} dB")
                })
                else EqNumber(translate("Preamp"), profile.manualPreampDb, -36.0..12.0, "dB", .1,
                    preview = { preview(EqEdit.Preamp(it)) }, commit = ::commit)
                val peak = view.peakDb.maxOrNull()
                if (!view.cast || peak != null) SettingsRow(translate(if (view.cast) "Encoding peak" else "Peak"), trailing = {
                    Text(if (clippingUntil > 0) translate("Clipping") else peak?.let { "${number(it)} dBFS" } ?: "-",
                        color = if (clippingUntil > 0) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
                })
            }
        } else Text(translate("Equalizer is controlled by the receiver"))
        if (receiverProcessing) FilledTonalButton({ outputOpen = true }, Modifier.fillMaxWidth(), colors = equalizerButtonColors()) {
            Text(remote?.name ?: translate("This device"), maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (!receiverProcessing) SettingsExpander(translate("Audio effects")) {
            Column {
                EqNumber(translate("Bass"), profile.bassDb, -12.0..12.0, "dB", .1, reset = true, preview = { preview(EqEdit.Bass(it)) }, commit = ::commit)
                EqNumber(translate("Treble"), profile.trebleDb, -12.0..12.0, "dB", .1, reset = true, preview = { preview(EqEdit.Treble(it)) }, commit = ::commit)
                SettingsSwitch(translate("Crossfeed"), view.crossfeed) { change(EqEdit.Crossfeed(it)) }
                EqNumber(translate("Balance"), view.balance, -1.0..1.0, "", .01, reset = true, preview = { preview(EqEdit.Balance(it)) }, commit = ::commit)
                SettingsSwitch(translate("Mono"), view.mono) { change(EqEdit.Mono(it)) }
            }
        }
        if (view.cast && !receiverProcessing) SettingsGroup {
            PreferenceChoice(translate("Encoding"), view.castFormat.name,
                listOf(EqCastFormat.MP3.name to "MP3", EqCastFormat.FLAC.name to "FLAC")) { change(EqEdit.CastFormat(EqCastFormat.valueOf(it))) }
            Button({ operation { owner.applyEqualizer(output) } }, enabled = view.pending || view.error != null) { Text(translate(if (view.error == null) "Apply" else "Retry")) }
        }
        view.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
    if (outputOpen) PlayerOutputSheet(player) { outputOpen = false }
    if (nameAction != null) PresetNameDialog(nameAction!!.name, name, { name = it }, { nameAction = null }) {
        operation {
            when (nameAction) {
                PresetNameAction.Save -> owner.saveEqualizerPreset(output, name)
                PresetNameAction.Duplicate -> owner.duplicateEqualizerPreset(output, name)
                PresetNameAction.Rename -> (view.presetId ?: view.saveTarget)?.let { owner.renameEqualizerPreset(it, name) }
                null -> Unit
            }
            nameAction = null
        }
    }
    transfer?.let { kind ->
        val importing = kind == PresetTransfer.Import
        PresetTransferDialog(importing, transferText, { text ->
            transferText = text
            transferError = null
            if (importing) transferFormat = if (text.trimStart().startsWith("{")) EqPresetFormat.JSON else EqPresetFormat.APO
        }, transferError, { transferError = it },
            "$displayName.${if (transferFormat == EqPresetFormat.JSON) "json" else "txt"}",
            if (transferFormat == EqPresetFormat.JSON) "application/json" else "text/plain",
            { operation {
                try { owner.importEqualizer(transferOutput, transferText, transferFormat); transfer = null }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { transferError = error.message }
            } }, { transfer = null }) {
            FilterChip(transferFormat == EqPresetFormat.JSON, { transferFormat = EqPresetFormat.JSON }, label = { Text("JSON") }, colors = equalizerChipColors())
            FilterChip(transferFormat == EqPresetFormat.APO, { transferFormat = EqPresetFormat.APO }, label = { Text("AutoEQ / APO") }, colors = equalizerChipColors())
        }
    }

}

@Composable
private fun equalizerControlColor() = MaterialTheme.colorScheme.onSurface.copy(alpha = .16f)
    .compositeOver(MaterialTheme.colorScheme.surfaceContainer)

@Composable
private fun equalizerButtonColors() = ButtonDefaults.filledTonalButtonColors(
    containerColor = equalizerControlColor(),
    contentColor = MaterialTheme.colorScheme.onSurface,
)

@Composable
private fun equalizerChipColors() = FilterChipDefaults.filterChipColors(
    containerColor = equalizerControlColor(),
    labelColor = MaterialTheme.colorScheme.onSurface,
    selectedContainerColor = MaterialTheme.colorScheme.primary,
    selectedLabelColor = MaterialTheme.colorScheme.onPrimary,
    disabledContainerColor = equalizerControlColor(),
    disabledSelectedContainerColor = equalizerControlColor(),
)

@Composable
internal fun EqualizerEditor(view: EqView, parametric: Boolean, frequencies: List<Double>, responseCurve: List<Double>,
    response: Boolean, spectrum: Boolean, onResponse: () -> Unit, onSpectrum: () -> Unit,
    preview: (EqEdit) -> Unit, commit: () -> Unit) {
    val bands = view.profile.bands
    var selected by rememberSaveable { mutableIntStateOf(0) }
    val index = selected.coerceIn(0, bands.lastIndex.coerceAtLeast(0))
    fun change(edit: EqEdit) { preview(edit); commit() }
    Surface(shape = RoundedCornerShape(18.dp), color = MaterialTheme.colorScheme.surfaceContainer) {
        Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(response, onResponse, label = { Text(translate("Response")) }, colors = equalizerChipColors())
                FilterChip(spectrum, onSpectrum, label = { Text(translate("Spectrum")) }, enabled = !view.cast, colors = equalizerChipColors())
                Spacer(Modifier.weight(1f))
                Box {
                    var layouts by remember { mutableStateOf(false) }
                    FilledTonalButton({ layouts = true }, contentPadding = PaddingValues(horizontal = 4.dp), colors = equalizerButtonColors()) {
                        Text(bands.size.toString())
                        io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(16.dp))
                    }
                    DropdownMenu(layouts, { layouts = false }) {
                        listOf(BandLayout.CLASSIC10 to "10 bands", BandLayout.BANDS18 to "18 bands", BandLayout.BANDS31 to "31 bands").forEach { (layout, label) ->
                            DropdownMenuItem(text = { Text(translate(label)) }, onClick = { layouts = false; change(EqEdit.Layout(layout)) })
                        }
                    }
                }
            }
            EqualizerPlot(view, parametric, frequencies, responseCurve, spectrum, index, { selected = it }, preview, commit)
            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            if (bands.isNotEmpty()) {
                val band = bands[index]
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    PlayerIconButton("rufin-go-previous-symbolic", "Previous", { selected = (index - 1).coerceAtLeast(0) }, enabled = index > 0)
                    Box(Modifier.weight(1f)) {
                        var bandList by remember { mutableStateOf(false) }
                        FilledTonalButton({ bandList = true }, Modifier.fillMaxWidth(), colors = equalizerButtonColors()) {
                            Text("${index + 1} · ${frequency(band.frequencyHz)} Hz", maxLines = 1)
                            io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(16.dp))
                        }
                        DropdownMenu(bandList, { bandList = false }, Modifier.heightIn(max = 320.dp)) {
                            bands.forEachIndexed { i, b -> DropdownMenuItem(text = { Text("${i + 1} · ${frequency(b.frequencyHz)} Hz   ${number(b.gainDb)} dB") }, onClick = { selected = i; bandList = false }) }
                        }
                    }
                    PlayerIconButton("rufin-go-next-symbolic", "Next", { selected = (index + 1).coerceAtMost(bands.lastIndex) }, enabled = index < bands.lastIndex)
                    if (parametric) PlayerIconButton("rufin-list-add-symbolic", "Add band", { selected = bands.size; change(EqEdit.AddBand) }, enabled = bands.size < 64)
                    else PlayerIconButton("rufin-edit-undo-symbolic", "Reset", { bands.indices.forEach { preview(EqEdit.BandGain(it.toUInt(), 0.0)) }; commit() })
                }
                if (parametric) Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.weight(1f)) {
                        var types by remember { mutableStateOf(false) }
                        FilledTonalButton({ types = true }, colors = equalizerButtonColors()) {
                            Text(translate(filterName(band.kind)))
                            io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(16.dp))
                        }
                        DropdownMenu(types, { types = false }) { FilterKind.entries.forEach { kind ->
                            DropdownMenuItem(text = { Text(translate(filterName(kind))) }, onClick = { types = false; change(EqEdit.BandKind(index.toUInt(), kind)) })
                        } }
                    }
                    Switch(band.enabled, { change(EqEdit.BandEnabled(index.toUInt(), it)) }, Modifier.semantics { contentDescription = translate("Band") })
                    PlayerIconButton("rufin-user-trash-symbolic", "Remove band", { change(EqEdit.RemoveBand(index.toUInt())) })
                }
                if (parametric) Row(Modifier.fillMaxWidth()) {
                    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(translate("Frequency"), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        EqValue(translate("Frequency"), band.frequencyHz, java.lang.Double.MIN_NORMAL..100000.0, "Hz") { change(EqEdit.BandFrequency(index.toUInt(), it)) }
                    }
                    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(translate("Width"), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            val q = band.width as? FilterWidth.Q
                            val width = q?.value ?: (band.width as FilterWidth.Hertz).value
                            EqValue(if (q != null) "Q" else translate("Width"), width, java.lang.Double.MIN_NORMAL..(if (q != null) Double.MAX_VALUE else 100000.0), "") {
                                change(EqEdit.BandWidth(index.toUInt(), if (q != null) FilterWidth.Q(it) else FilterWidth.Hertz(it)))
                            }
                            Box {
                                var units by remember { mutableStateOf(false) }
                                FilledTonalButton({ units = true }, contentPadding = PaddingValues(horizontal = 4.dp), colors = equalizerButtonColors()) {
                                    Text(if (q != null) "Q" else "Hz")
                                    io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(16.dp))
                                }
                                DropdownMenu(units, { units = false }) {
                                    listOf(true to "Q", false to "Hz").forEach { (useQ, unit) ->
                                        DropdownMenuItem(text = { Text(unit) }, onClick = { units = false; change(EqEdit.BandWidthUnit(index.toUInt(), useQ)) })
                                    }
                                }
                            }
                        }
                    }
                }
                EqNumber(translate("Gain"), band.gainDb, -24.0..12.0, "dB", .1,
                    preview = { preview(EqEdit.BandGain(index.toUInt(), it)) }, commit = commit)
                if (view.sampleRate != null && band.frequencyHz >= view.sampleRate!!.toDouble() / 2) Text(translate("Inactive at this sample rate"), style = MaterialTheme.typography.labelSmall)
            } else TextButton({ change(EqEdit.AddBand) }) { Text(translate("Add band")) }
        }
    }
}

private fun filterName(kind: FilterKind) = when (kind) {
    FilterKind.PEAK -> "Peak"
    FilterKind.LOW_SHELF -> "Low shelf"
    FilterKind.HIGH_SHELF -> "High shelf"
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EqNumber(title: String, value: Double, range: ClosedFloatingPointRange<Double>, unit: String, step: Double, reset: Boolean = false,
    preview: (Double) -> Unit, commit: () -> Unit) {
    var draft by remember(value) { mutableDoubleStateOf(value) }
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainer).padding(horizontal = 12.dp, vertical = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
            if (reset) {
                var text by remember(value) { mutableStateOf(number(value)) }
                OutlinedTextField(text, { input ->
                    text = input
                    input.replace(',', '.').toDoubleOrNull()?.takeIf { it.isFinite() && it in range }?.let { preview(it); commit() }
                }, singleLine = true, colors = settingsTextFieldColors(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                    suffix = if (unit.isEmpty()) null else { { Text(unit) } },
                    trailingIcon = if (value == 0.0) null else { {
                        PlayerIconButton("rufin-view-refresh-symbolic", "Reset", { text = "0"; preview(0.0); commit() })
                    } },
                    modifier = Modifier.width(132.dp).semantics { contentDescription = title })
            } else EqValue(title, draft, range, unit) { preview(it); commit() }
        }
        Slider(draft.toFloat(), {
            draft = (round(it / step) * step).coerceIn(range)
            preview(draft)
        }, valueRange = range.start.toFloat()..range.endInclusive.toFloat(), onValueChangeFinished = commit,
            thumb = { Box(Modifier.size(16.dp).background(MaterialTheme.colorScheme.primary, androidx.compose.foundation.shape.CircleShape)) },
            track = { state -> SliderDefaults.Track(state, modifier = Modifier.height(4.dp), thumbTrackGapSize = 0.dp, drawStopIndicator = null) },
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = title; stateDescription = "${number(draft)} $unit" })
        if (reset) SliderMarks(range.start.toFloat()..range.endInclusive.toFloat(),
            listOf(range.start.toFloat(), (range.start / 2).toFloat(), 0f, (range.endInclusive / 2).toFloat(), range.endInclusive.toFloat()))
    }
}

@Composable
private fun EqValue(title: String, value: Double, range: ClosedFloatingPointRange<Double>, unit: String, change: (Double) -> Unit) {
    var editing by remember { mutableStateOf(false) }
    var text by remember { mutableStateOf("") }
    TextButton({ text = value.toString(); editing = true }, contentPadding = PaddingValues(horizontal = 4.dp),
        modifier = Modifier.semantics { contentDescription = title }) { Text("${number(value)} $unit".trim(), maxLines = 1) }
    if (editing) AlertDialog(onDismissRequest = { editing = false }, title = { Text(title) }, text = {
        OutlinedTextField(text, { text = it }, singleLine = true, colors = settingsTextFieldColors(), keyboardOptions = KeyboardOptions(keyboardType = if (range.start < 0) KeyboardType.Text else KeyboardType.Decimal))
    }, confirmButton = { TextButton({ text.replace(',', '.').toDoubleOrNull()?.let(change); editing = false }, enabled = text.replace(',', '.').toDoubleOrNull()?.let { it.isFinite() && it in range } == true) { Text(translate("Apply")) } },
        dismissButton = { TextButton({ editing = false }) { Text(translate("Cancel")) } })
}

private fun number(value: Double): String = when {
    !value.isFinite() -> "-"
    value != 0.0 && (abs(value) < .001 || abs(value) > 100000.0) -> String.format(Locale.getDefault(), "%.3g", value)
    else -> String.format(Locale.getDefault(), "%.3f", value).trimEnd('0').trimEnd('.', ',')
}
private fun frequency(value: Double): String = if (value >= 1000) "${number(value / 1000)}k" else number(value)

@Composable
private fun EqualizerPlot(view: EqView, parametric: Boolean, frequencies: List<Double>, response: List<Double>, spectrum: Boolean,
    selected: Int, select: (Int) -> Unit, preview: (EqEdit) -> Unit, commit: () -> Unit) {
    val bands = view.profile.bands
    val colors = MaterialTheme.colorScheme
    val textMeasurer = rememberTextMeasurer()
    val labelStyle = MaterialTheme.typography.labelSmall.copy(color = colors.onSurfaceVariant, fontSize = 10.sp)
    var dragRange by remember { mutableStateOf<Pair<Double, Double>?>(null) }
    val low = dragRange?.first ?: min(-24.0, floor((response.minOrNull() ?: -24.0) / 12.0) * 12.0)
    val high = dragRange?.second ?: max(12.0, ceil((response.maxOrNull() ?: 12.0) / 12.0) * 12.0)
    val lowHz = min(20.0, bands.minOfOrNull { it.frequencyHz } ?: 20.0)
    val highHz = max(20000.0, bands.maxOfOrNull { it.frequencyHz } ?: 20000.0)
    val logLow = ln(lowHz)
    val logSpan = ln(highHz) - logLow
    val order = remember(bands, parametric) {
        if (parametric) bands.indices.toList() else bands.indices.sortedBy { bands[it].frequencyHz }
    }
    val graphicPoints = remember(bands, order) {
        order.withIndex().groupBy { bands[it.value].frequencyHz }.map { (hz, columns) ->
            hz to ((columns.first().index + columns.last().index + 1.0) / (2.0 * bands.size)).toFloat()
        }
    }
    fun position(hz: Double): Float {
        if (parametric || graphicPoints.size < 2) return ((ln(hz.coerceIn(lowHz, highHz)) - logLow) / logSpan).toFloat()
        val upper = graphicPoints.indexOfFirst { it.first >= hz }.let { if (it < 0) graphicPoints.lastIndex else it.coerceAtLeast(1) }
        val (lowFrequency, lowPosition) = graphicPoints[upper - 1]
        val (highFrequency, highPosition) = graphicPoints[upper]
        val ratio = ln(hz / lowFrequency) / ln(highFrequency / lowFrequency)
        return (lowPosition + (highPosition - lowPosition) * ratio).toFloat().coerceIn(0f, 1f)
    }
    val latestView by rememberUpdatedState(view)
    val latestLow by rememberUpdatedState(low)
    val latestHigh by rememberUpdatedState(high)
    val latestLogSpan by rememberUpdatedState(logSpan)
    val latestOrder by rememberUpdatedState(order)
    val latestPosition by rememberUpdatedState<(Double) -> Float>(::position)
    val latestSelect by rememberUpdatedState(select)
    val latestPreview by rememberUpdatedState(preview)
    val latestCommit by rememberUpdatedState(commit)
    Canvas(Modifier.fillMaxWidth().height(244.dp)
        .pointerInput(parametric, bands.size) {
            val left = 28.dp.toPx()
            val top = 12.dp.toPx()
            val width = (size.width - left - 8.dp.toPx()).coerceAtLeast(1f)
            val height = (size.height - top - 24.dp.toPx()).coerceAtLeast(1f)
            fun nearest(point: Offset): Int? = latestOrder.withIndex().minByOrNull { (column, i) ->
                val band = latestView.profile.bands[i]
                val x = left + (if (parametric) latestPosition(band.frequencyHz) else (column + .5f) / latestOrder.size) * width
                val y = top + ((latestHigh - band.gainDb) / (latestHigh - latestLow) * height).toFloat()
                if (parametric) (point - Offset(x, y)).getDistanceSquared() else abs(point.x - x)
            }?.value
            var index = -1
            var gain = 0.0
            var hz = 1000.0
            var span = 36.0
            detectDragGestures(onDragStart = { point ->
                index = nearest(point) ?: -1
                if (index >= 0) {
                    latestSelect(index)
                    gain = latestView.profile.bands[index].gainDb
                    hz = latestView.profile.bands[index].frequencyHz
                    span = latestHigh - latestLow
                    dragRange = latestLow to latestHigh
                }
            }, onDragEnd = { if (index >= 0) latestCommit(); index = -1; dragRange = null },
                onDragCancel = { if (index >= 0) latestCommit(); index = -1; dragRange = null }) { change, delta ->
                if (index >= 0) {
                    change.consume()
                    gain = (gain - delta.y / height * span).coerceIn(-24.0, 12.0)
                    if (parametric) {
                        hz = (hz * exp(delta.x / width * latestLogSpan)).coerceIn(java.lang.Double.MIN_NORMAL, 100000.0)
                        latestPreview(EqEdit.BandFrequency(index.toUInt(), hz))
                    }
                    latestPreview(EqEdit.BandGain(index.toUInt(), gain))
                }
            }
        }
        .pointerInput(parametric, bands.size) {
            detectTapGestures { point ->
                val left = 28.dp.toPx()
                val width = (size.width - left - 8.dp.toPx()).coerceAtLeast(1f)
                val height = (size.height - 36.dp.toPx()).coerceAtLeast(1f)
                latestOrder.withIndex().minByOrNull { (column, i) ->
                    val band = latestView.profile.bands[i]
                    val x = left + (if (parametric) latestPosition(band.frequencyHz) else (column + .5f) / latestOrder.size) * width
                    val y = 12.dp.toPx() + ((latestHigh - band.gainDb) / (latestHigh - latestLow) * height).toFloat()
                    if (parametric) (point - Offset(x, y)).getDistanceSquared() else abs(point.x - x)
                }?.value?.let(latestSelect)
            }
        }
    ) {
        val left = 28.dp.toPx()
        val top = 12.dp.toPx()
        val width = size.width - left - 8.dp.toPx()
        val height = size.height - top - 24.dp.toPx()
        fun y(gain: Double) = top + ((high - gain) / (high - low) * height).toFloat()
        fun x(hz: Double) = left + position(hz) * width
        for (gain in listOf(high, 0.0, low)) {
            val py = y(gain)
            drawLine(colors.outlineVariant, Offset(left, py), Offset(left + width, py))
            val text = textMeasurer.measure(number(gain), labelStyle)
            drawText(text, topLeft = Offset(0f, py - text.size.height / 2))
        }
        if (spectrum && view.sampleRate != null && view.spectrumDb.isNotEmpty()) {
            val path = Path()
            view.spectrumDb.forEachIndexed { i, db ->
                val hz = i.toDouble() * view.sampleRate!!.toDouble() / (2.0 * (view.spectrumDb.size - 1).coerceAtLeast(1))
                if (hz in lowHz..highHz) {
                    val px = x(hz)
                    val py = top + (-db.coerceIn(-96.0, 0.0) / 96.0 * height).toFloat()
                    if (path.isEmpty) path.moveTo(px, py) else path.lineTo(px, py)
                }
            }
            drawPath(path, colors.tertiary.copy(alpha = .5f), style = Stroke(1.dp.toPx()))
            val text = textMeasurer.measure("dBFS", labelStyle)
            drawText(text, topLeft = Offset(size.width - text.size.width, 0f))
        }
        if (response.size == frequencies.size) {
            val path = Path()
            frequencies.forEachIndexed { i, hz ->
                if (view.sampleRate != null && hz > view.sampleRate!!.toDouble() / 2.0) return@forEachIndexed
                val px = x(hz)
                val py = y(response[i]).coerceIn(top, top + height)
                if (path.isEmpty) path.moveTo(px, py) else path.lineTo(px, py)
            }
            drawPath(path, colors.secondary, style = Stroke(2.dp.toPx()))
        }
        val gains = Path()
        val labelStep = ceil(bands.size / 5.0).toInt().coerceAtLeast(1)
        val lastLabel = order.lastOrNull()?.let { textMeasurer.measure(frequency(bands[it].frequencyHz), labelStyle) }
        val lastLabelStart = size.width - (lastLabel?.size?.width ?: 0)
        var labelEnd = 0f
        order.forEachIndexed { column, i ->
            val band = bands[i]
            val px = if (parametric) x(band.frequencyHz) else left + (column + .5f) / bands.size * width
            val py = y(band.gainDb)
            val active = band.enabled && (view.sampleRate == null || band.frequencyHz < view.sampleRate!!.toDouble() / 2.0)
            if (!parametric) {
                drawLine(if (i == selected) colors.primary.copy(alpha = .5f) else colors.outlineVariant.copy(alpha = .6f), Offset(px, top), Offset(px, top + height), 1.dp.toPx())
                if (column == 0) gains.moveTo(px, py) else gains.lineTo(px, py)
                if (column % labelStep == 0 || column == bands.lastIndex) {
                    val text = textMeasurer.measure(frequency(band.frequencyHz), labelStyle)
                    val labelX = (px - text.size.width / 2).coerceIn(left, (size.width - text.size.width).coerceAtLeast(left))
                    if (column == bands.lastIndex || (labelX >= labelEnd && labelX + text.size.width + 8.dp.toPx() < lastLabelStart)) {
                        drawText(text, topLeft = Offset(labelX, top + height + 6.dp.toPx()))
                        labelEnd = labelX + text.size.width + 8.dp.toPx()
                    }
                }
            }
            drawCircle(if (active) colors.primary else colors.outline, (if (i == selected) 6.dp else 3.dp).toPx(), Offset(px, py))
        }
        if (!parametric) drawPath(gains, colors.primary.copy(alpha = .7f), style = Stroke(1.5.dp.toPx()))
        else listOf(lowHz, 100.0, 1000.0, highHz).distinct().filter { it in lowHz..highHz }.forEach { hz ->
            val text = textMeasurer.measure(frequency(hz), labelStyle)
            drawText(text, topLeft = Offset((x(hz) - text.size.width / 2).coerceIn(left, (size.width - text.size.width).coerceAtLeast(left)), top + height + 6.dp.toPx()))
        }
    }
}

@Composable
internal fun PresetNameDialog(title: String, name: String, onName: (String) -> Unit, dismiss: () -> Unit, save: () -> Unit) {
    AlertDialog(onDismissRequest = dismiss,
        title = { Text(translate(title)) }, text = {
            OutlinedTextField(name, onName, label = { Text(translate("Name")) }, singleLine = true, colors = settingsTextFieldColors())
        }, confirmButton = { TextButton(save, enabled = name.isNotBlank()) { Text(translate("Save")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@Composable
internal fun PresetTransferDialog(importing: Boolean, text: String, onText: (String) -> Unit,
    error: String?, onError: (String?) -> Unit, filename: String, mimeType: String,
    importPreset: () -> Unit, dismiss: () -> Unit, formats: @Composable RowScope.() -> Unit = {}) {
    val context = LocalContext.current
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val scope = rememberCoroutineScope()
    var documentText by rememberSaveable { mutableStateOf("") }
    fun operation(block: suspend () -> Unit) = scope.launch {
        try { block() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { onError(failure.message) }
    }
    val open = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) operation {
            val contents = withContext(Dispatchers.IO) {
                context.contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
                    ?: kotlin.error(translate("Import"))
            }
            onText(contents)
        }
    }
    val save = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(mimeType)) { uri ->
        if (uri != null) operation {
            withContext(Dispatchers.IO) {
                val stream = context.contentResolver.openOutputStream(uri, "wt") ?: kotlin.error(translate("Export"))
                stream.bufferedWriter().use { it.write(documentText) }
            }
        }
    }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate(if (importing) "Import preset" else "Export preset")) }, text = {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                formats()
                if (importing) PlayerIconButton("rufin-document-open-symbolic", "Open", {
                    open.launch(arrayOf("application/json", "text/plain", "application/octet-stream"))
                })
            }
            OutlinedTextField(text, onText, readOnly = !importing,
                modifier = Modifier.fillMaxWidth().height(200.dp), colors = settingsTextFieldColors(),
                textStyle = MaterialTheme.typography.bodySmall.copy(fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace))
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }, confirmButton = {
        if (importing) TextButton(importPreset, enabled = text.isNotBlank()) { Text(translate("Import")) }
        else TextButton({ documentText = text; save.launch(filename) },
            enabled = error == null && text.isNotBlank()) { Text(translate("Save")) }
    }, dismissButton = {
        if (importing) TextButton({
            onText(clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty())
        }) { Text(translate("Paste")) }
        else TextButton({ clipboard.setPrimaryClip(ClipData.newPlainText(filename, text)); dismiss() },
            enabled = error == null && text.isNotBlank()) { Text(translate("Copy")) }

    })
}

@Composable
internal fun SliderMarks(range: ClosedFloatingPointRange<Float>, labels: List<Float>) {
    val measurer = rememberTextMeasurer()
    val color = MaterialTheme.colorScheme.onSurfaceVariant
    val style = MaterialTheme.typography.labelSmall.copy(color = color)
    Canvas(Modifier.fillMaxWidth().height(26.dp)) {
        val inset = 10.dp.toPx()
        fun x(value: Float) = inset + (size.width - 2 * inset) * (value - range.start) / (range.endInclusive - range.start)
        val ticks = labels.zipWithNext().flatMap { (start, end) -> (0..3).map { start + (end - start) * it / 4 } } + labels.last()
        ticks.forEach { drawLine(color.copy(alpha = .45f), Offset(x(it), 0f), Offset(x(it), (if (it in labels) 5.dp else 3.dp).toPx()), 1.dp.toPx()) }
        labels.forEach {
            val label = measurer.measure(number(it.toDouble()), style)
            drawText(label, topLeft = Offset((x(it) - label.size.width / 2).coerceIn(0f, (size.width - label.size.width).coerceAtLeast(0f)), 5.dp.toPx()))
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun PresetActions(save: () -> Unit, duplicate: () -> Unit, rename: () -> Unit,
    importPreset: () -> Unit, exportPreset: () -> Unit, delete: () -> Unit,
    saveEnabled: Boolean = true, editEnabled: Boolean = true, modifier: Modifier = Modifier) {
    FlowRow(modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween,
        verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(translate("Presets"), Modifier.align(Alignment.CenterVertically), style = MaterialTheme.typography.titleSmall)
        FlowRow(Modifier.widthIn(max = 288.dp), horizontalArrangement = Arrangement.End) {
            PlayerIconButton("rufin-document-save-symbolic", "Save", save, modifier = Modifier.size(48.dp), enabled = saveEnabled)
            PlayerIconButton("rufin-edit-copy-symbolic", "Duplicate", duplicate, modifier = Modifier.size(48.dp))
            PlayerIconButton("rufin-document-edit-symbolic", "Rename", rename, modifier = Modifier.size(48.dp), enabled = editEnabled)
            PlayerIconButton("rufin-document-open-symbolic", "Import", importPreset, modifier = Modifier.size(48.dp))
            PlayerIconButton("rufin-document-send-symbolic", "Export", exportPreset, modifier = Modifier.size(48.dp))
            PlayerIconButton("rufin-user-trash-symbolic", "Delete", delete, modifier = Modifier.size(48.dp), enabled = editEnabled)
        }
    }
}
