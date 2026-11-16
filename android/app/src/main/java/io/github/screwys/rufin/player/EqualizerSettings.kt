package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.RufinSlider

import io.github.screwys.rufin.ui.ChoiceChip

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.SystemClock
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.drag
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.ui.input.pointer.positionChange
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.draw.clip
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
                // The output and the graphic/parametric switch share a line; whether this output keeps
                // the equalizer sits under them.
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilledTonalButton({ outputOpen = true }, Modifier.weight(1f), colors = equalizerButtonColors()) {
                        io.github.screwys.rufin.ui.RufinIcon("rufin-speaker-front-symbolic", null, Modifier.size(18.dp))
                        Spacer(Modifier.width(8.dp))
                        Text(remote?.name ?: routes.firstOrNull { it.selected }?.name ?: translate("This device"),
                            Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        io.github.screwys.rufin.ui.RufinIcon("rufin-go-down-symbolic", null, Modifier.size(18.dp))
                    }
                    EqualizerModeSwitch(editingParametric) { parametric = it }
                }
                // A switch, as the app's other on-off settings.
                Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp))
                    .toggleable(view.assigned, role = Role.Switch) { assigned -> operation { owner.assignEqualizer(output, assigned) } }
                    .padding(start = 4.dp, top = 2.dp, bottom = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(translate("Use for this output"), Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                    Switch(view.assigned, null)
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
                else EqNumber(translate("Preamp"), profile.manualPreampDb, -36.0..12.0, "dB", .1, listOf(-36.0, -24.0, -12.0, 0.0, 12.0),
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
        if (!receiverProcessing) SettingsGroup(translate("Audio effects")) {
            val tone = listOf(-12.0, -6.0, 0.0, 6.0, 12.0)
            EqNumber(translate("Bass"), profile.bassDb, -12.0..12.0, "dB", .1, tone, preview = { preview(EqEdit.Bass(it)) }, commit = ::commit)
            EqNumber(translate("Treble"), profile.trebleDb, -12.0..12.0, "dB", .1, tone, preview = { preview(EqEdit.Treble(it)) }, commit = ::commit)
            EqNumber(translate("Balance"), view.balance, -1.0..1.0, "", .01, listOf(-1.0, -.5, 0.0, .5, 1.0), preview = { preview(EqEdit.Balance(it)) }, commit = ::commit)
            Row(horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                EqToggle(translate("Crossfeed"), view.crossfeed, Modifier.weight(1f)) { change(EqEdit.Crossfeed(it)) }
                EqToggle(translate("Mono"), view.mono, Modifier.weight(1f)) { change(EqEdit.Mono(it)) }
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
            io.github.screwys.rufin.ui.SegmentedPill(listOf(EqPresetFormat.JSON, EqPresetFormat.APO), transferFormat, { transferFormat = it },
                Modifier.weight(1f), equal = true) { format, _ ->
                Text(if (format == EqPresetFormat.JSON) "JSON" else "AutoEQ / APO", style = MaterialTheme.typography.titleSmall, maxLines = 1)
            }
        }
    }

}

@Composable
private fun equalizerControlColor() = MaterialTheme.colorScheme.onSurface.copy(alpha = .16f)
    .compositeOver(MaterialTheme.colorScheme.surfaceContainer)

/** Graphic and parametric editing: two icon choices in one box, a line between them. */
@Composable
private fun EqualizerModeSwitch(parametric: Boolean, select: (Boolean) -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(shape = RoundedCornerShape(20.dp), color = equalizerControlColor()) {
        Row(Modifier.height(40.dp).padding(3.dp).selectableGroup(), verticalAlignment = Alignment.CenterVertically) {
            listOf(Triple(false, "rufin-sliders-vertical-symbolic", "Graphic"), Triple(true, "rufin-spring-graph-symbolic", "Parametric"))
                .forEachIndexed { index, (value, icon, label) ->
                    if (index > 0) Box(Modifier.padding(horizontal = 2.dp).width(1.dp).height(18.dp).background(colors.onSurface.copy(alpha = .2f)))
                    val selected = parametric == value
                    Box(Modifier.fillMaxHeight().width(44.dp).clip(RoundedCornerShape(17.dp))
                        .background(if (selected) colors.secondaryContainer else Color.Transparent)
                        .selectable(selected, role = Role.RadioButton) { select(value) }, contentAlignment = Alignment.Center) {
                        io.github.screwys.rufin.ui.RufinIcon(icon, translate(label), Modifier.size(20.dp),
                            if (selected) colors.onSecondaryContainer else colors.onSurfaceVariant)
                    }
                }
        }
    }
}

@Composable
private fun equalizerButtonColors() = ButtonDefaults.filledTonalButtonColors(
    containerColor = equalizerControlColor(),
    contentColor = MaterialTheme.colorScheme.onSurface,
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
            FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row(Modifier.align(Alignment.CenterVertically), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceChip(response, onResponse, label = { Text(translate("Response")) })
                    ChoiceChip(spectrum, onSpectrum, label = { Text(translate("Spectrum")) }, enabled = !view.cast)
                }
                // The graphic layouts by their band count; a parametric profile picks none of them.
                io.github.screwys.rufin.ui.SegmentedPill(listOf(BandLayout.CLASSIC10, BandLayout.BANDS18, BandLayout.BANDS31), view.profile.layout,
                    { change(EqEdit.Layout(it)) }) { layout, _ ->
                    Text(when (layout) { BandLayout.CLASSIC10 -> "10"; BandLayout.BANDS18 -> "18"; else -> "31" },
                        Modifier.semantics { contentDescription = translate(when (layout) { BandLayout.CLASSIC10 -> "10 bands"; BandLayout.BANDS18 -> "18 bands"; else -> "31 bands" }) },
                        style = MaterialTheme.typography.labelLarge)
                }
            }
            EqualizerPlot(view, parametric, frequencies, responseCurve, spectrum, index, { selected = it }, preview, commit)
            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            if (bands.isNotEmpty()) {
                val band = bands[index]
                // Every band as a button with its frequency and gain; the picked one is the one the rows under it edit.
                val strip = rememberLazyListState()
                val control = equalizerControlColor()
                LaunchedEffect(index, bands.size) {
                    val shown = strip.layoutInfo.visibleItemsInfo.any { it.index == index && it.offset >= 0 && it.offset + it.size <= strip.layoutInfo.viewportEndOffset }
                    if (!shown) strip.animateScrollToItem((index - 2).coerceAtLeast(0))
                }
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    LazyRow(Modifier.weight(1f).selectableGroup(), state = strip, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        itemsIndexed(bands) { i, b ->
                            val picked = i == index
                            val scheme = MaterialTheme.colorScheme
                            Column(Modifier.width(50.dp).height(46.dp).clip(RoundedCornerShape(12.dp))
                                .background(if (picked) scheme.primary else control)
                                .selectable(picked, role = Role.Tab) { selected = i },
                                horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
                                Text(frequency(b.frequencyHz), maxLines = 1, style = MaterialTheme.typography.labelMedium, fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
                                    color = if (picked) scheme.onPrimary else scheme.onSurface)
                                Text(number(b.gainDb), maxLines = 1, style = MaterialTheme.typography.labelSmall,
                                    color = if (picked) scheme.onPrimary else scheme.onSurfaceVariant)
                            }
                        }
                    }
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
                EqNumber(translate("Gain"), band.gainDb, -24.0..12.0, "dB", .1, listOf(-24.0, -12.0, 0.0, 12.0),
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

/** A level that goes both ways from zero: its value opens exact entry, Reset shows once it leaves zero, and the
 *  slider fills from zero and sticks to the marks under it, which jump to their value when tapped. */
@Composable
private fun EqNumber(title: String, value: Double, range: ClosedFloatingPointRange<Double>, unit: String, step: Double,
    marks: List<Double>, preview: (Double) -> Unit, commit: () -> Unit) {
    var draft by remember(value) { mutableDoubleStateOf(value) }
    val stops = remember(marks) { marks.map { it.toFloat() } }
    val span = range.start.toFloat()..range.endInclusive.toFloat()
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(5.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(horizontal = 12.dp, vertical = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
            if (draft != 0.0) PlayerIconButton("rufin-view-refresh-symbolic", "Reset", { draft = 0.0; preview(0.0); commit() }, iconSize = 18.dp)
            EqValue(title, draft, range, unit) { preview(it); commit() }
        }
        RufinSlider(draft.toFloat(), {
            draft = (round(it / step) * step).coerceIn(range)
            preview(draft)
        }, Modifier.fillMaxWidth().semantics { contentDescription = title; stateDescription = "${number(draft)} $unit" },
            valueRange = span, onValueChangeFinished = commit, marks = stops, origin = 0f)
        SliderMarks(span, stops, default = 0f) { draft = it.toDouble(); preview(draft); commit() }
    }
}

/** An effect that is on or off, as half of a row. */
@Composable
private fun EqToggle(label: String, checked: Boolean, modifier: Modifier, change: (Boolean) -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Row(modifier.clip(RoundedCornerShape(5.dp)).background(if (checked) scheme.secondaryContainer else scheme.surfaceContainer)
        .toggleable(checked, role = Role.Switch, onValueChange = change).heightIn(min = 56.dp).padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge, color = if (checked) scheme.primary else scheme.onSurface)
        Box(Modifier.size(12.dp).clip(androidx.compose.foundation.shape.CircleShape)
            .background(if (checked) scheme.primary else scheme.onSurfaceVariant.copy(alpha = .35f)))
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
            val reach = 28.dp.toPx()
            // A drag moves a band only from that band's own handle: anywhere else on the plot the page scrolls.
            fun grabbed(point: Offset): Int? = latestOrder.withIndex().map { (column, i) ->
                val band = latestView.profile.bands[i]
                val x = left + (if (parametric) latestPosition(band.frequencyHz) else (column + .5f) / latestOrder.size) * width
                val y = top + ((latestHigh - band.gainDb) / (latestHigh - latestLow) * height).toFloat()
                i to (point - Offset(x, y)).getDistance()
            }.filter { it.second <= reach }.minByOrNull { it.second }?.first
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false)
                val index = grabbed(down.position) ?: return@awaitEachGesture
                latestSelect(index)
                var gain = latestView.profile.bands[index].gainDb
                var hz = latestView.profile.bands[index].frequencyHz
                val span = latestHigh - latestLow
                var moved = false
                dragRange = latestLow to latestHigh
                drag(down.id) { change ->
                    val delta = change.positionChange()
                    change.consume()
                    moved = true
                    gain = (gain - delta.y / height * span).coerceIn(-24.0, 12.0)
                    if (parametric) {
                        hz = (hz * exp(delta.x / width * latestLogSpan)).coerceIn(java.lang.Double.MIN_NORMAL, 100000.0)
                        latestPreview(EqEdit.BandFrequency(index.toUInt(), hz))
                    }
                    latestPreview(EqEdit.BandGain(index.toUInt(), gain))
                }
                if (moved) latestCommit()
                dragRange = null
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
            drawLine(if (gain == 0.0) colors.outline else colors.outlineVariant, Offset(left, py), Offset(left + width, py))
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
            var first = 0f
            var last = 0f
            frequencies.forEachIndexed { i, hz ->
                if (view.sampleRate != null && hz > view.sampleRate!!.toDouble() / 2.0) return@forEachIndexed
                val px = x(hz)
                val py = y(response[i]).coerceIn(top, top + height)
                if (path.isEmpty) { path.moveTo(px, py); first = px } else path.lineTo(px, py)
                last = px
            }
            // The response as a soft area off the zero line, so boost and cut read at a glance.
            if (!path.isEmpty) drawPath(Path().apply {
                addPath(path)
                lineTo(last, y(0.0)); lineTo(first, y(0.0)); close()
            }, colors.primary.copy(alpha = .14f))
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
            if (parametric && i == selected) drawLine(colors.primary.copy(alpha = .5f), Offset(px, top), Offset(px, top + height), 1.dp.toPx())
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
            // The picked band's handle is the one sized for a finger, ringed off the lines under it.
            if (i == selected) drawCircle(colors.surfaceContainer, 11.dp.toPx(), Offset(px, py))
            drawCircle(if (active) colors.primary else colors.outline, (if (i == selected) 8.dp else 3.5.dp).toPx(), Offset(px, py))
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
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
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

/** The marks under a slider. `default` names the one the slider rests on; with `pick`, a tap takes the nearest. */
@Composable
internal fun SliderMarks(range: ClosedFloatingPointRange<Float>, labels: List<Float>, default: Float? = null, pick: ((Float) -> Unit)? = null) {
    val measurer = rememberTextMeasurer()
    val color = MaterialTheme.colorScheme.onSurfaceVariant
    val accent = MaterialTheme.colorScheme.primary
    val style = MaterialTheme.typography.labelSmall.copy(color = color)
    val latestPick by rememberUpdatedState(pick)
    Canvas(Modifier.fillMaxWidth().height(26.dp).then(if (pick == null) Modifier else Modifier.pointerInput(range, labels) {
        detectTapGestures { point ->
            val inset = 10.dp.toPx()
            labels.minByOrNull { abs(inset + (size.width - 2 * inset) * (it - range.start) / (range.endInclusive - range.start) - point.x) }
                ?.let { latestPick?.invoke(it) }
        }
    })) {
        val inset = 10.dp.toPx()
        fun x(value: Float) = inset + (size.width - 2 * inset) * (value - range.start) / (range.endInclusive - range.start)
        val ticks = labels.zipWithNext().flatMap { (start, end) -> (0..3).map { start + (end - start) * it / 4 } } + labels.last()
        ticks.forEach { drawLine(color.copy(alpha = .45f), Offset(x(it), 0f), Offset(x(it), (if (it in labels) 5.dp else 3.dp).toPx()), 1.dp.toPx()) }
        labels.forEach {
            val label = measurer.measure(number(it.toDouble()), if (it == default) style.copy(color = accent, fontWeight = androidx.compose.ui.text.font.FontWeight.Bold) else style)
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
