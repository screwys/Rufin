package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.PagerTabIndicator
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.SettingsSwitch
import io.github.screwys.rufin.settings.SettingsReorderRows
import io.github.screwys.rufin.settings.SettingsReorderItem
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.SettingsSliderNumber
import io.github.screwys.rufin.settings.settingsTextFieldColors

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale
import kotlin.math.round

internal enum class PlayerSettingsPage(val title: String) {
    PREFERENCES("Preferences"), EQUALIZER("Equalizer"), LYRICS("Lyrics"), VISUALIZER("Visualizer")
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlayerSettingsSheet(player: PlayerConnection, preferences: PreferencesConnection, initial: PlayerSettingsPage, onDismiss: () -> Unit) {
    val pager = rememberPagerState(initial.ordinal) { PlayerSettingsPage.entries.size }
    var visited by remember { mutableStateOf(setOf(initial.ordinal)) }
    val settledPage by remember { derivedStateOf { pager.settledPage } }
    val selectedPage by remember { derivedStateOf { pager.targetPage } }
    LaunchedEffect(settledPage) { visited = visited + settledPage }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.72f)) {
            PrimaryScrollableTabRow(selectedPage, indicator = { PagerTabIndicator(pager) }) {
                PlayerSettingsPage.entries.forEachIndexed { index, page ->
                    Tab(selectedPage == index, { scope.launch { if (reduceMotion) pager.scrollToPage(index) else pager.animateScrollToPage(index) } }, text = { Text(translate(page.title)) })
                }
            }
            HorizontalPager(pager, Modifier.fillMaxWidth().weight(1f), verticalAlignment = Alignment.Top,
                beyondViewportPageCount = PlayerSettingsPage.entries.size - 1) { index ->
                Box(Modifier.fillMaxSize()) {
                    if (index in visited) CompositionLocalProvider(LocalPlayerBottomPadding provides 0.dp) {
                        PlayerSettingsContent(player, PlayerSettingsPage.entries[index], preferences = preferences)
                    }
                }
            }
        }
    }
}

@Composable
internal fun PlayerSettingsContent(player: PlayerConnection, page: PlayerSettingsPage, embedded: Boolean = false, preferences: PreferencesConnection? = null) {
    SettingsPage(embedded = embedded) {
        if (player.settings == null) Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        else when (page) {
            PlayerSettingsPage.EQUALIZER -> EqualizerSettings(player)
            PlayerSettingsPage.LYRICS -> LyricsSettings(player)
            PlayerSettingsPage.VISUALIZER -> VisualizerSettings(player)
            PlayerSettingsPage.PREFERENCES -> preferences?.let { PlaybackPreferences(it) }
        }
    }
}

@Composable
private fun PlaybackPreferences(preferences: PreferencesConnection) {
    val state = preferences.applicationSettings ?: return
    val playback = state.getJSONObject("playback")
    fun change(field: String, value: Any) { preferences.changePreference(field, value) }
    SettingsGroup {
        val transition = playback.getString("transition_mode")
        PreferenceChoice(translate("Transition mode"), transition,
            listOf("Gapless" to translate("Gapless"), "Crossfade" to translate("Crossfade")),
            select = { change("playback.transition_mode", it) })
        if (transition == "Crossfade") SettingsSliderNumber(translate("Crossfade duration"), playback.getDouble("crossfade_seconds"), 1.0..30.0, 1.0, integer = true, unit = "s") {
            change("playback.crossfade_seconds", it)
        }
        PreferenceSwitch("Skip same-album crossfade", playback.getBoolean("skip_same_album_crossfade")) { change("playback.skip_same_album_crossfade", it) }
        PreferenceSwitch("Audio fade on play/pause", playback.getBoolean("audio_fade_on_status_change")) { change("playback.audio_fade_on_status_change", it) }
        SettingsSliderNumber(translate("Playback speed"), playback.getDouble("playback_rate"), .5..2.0, .05, unit = "×") { change("playback.playback_rate", it) }
        PreferenceSwitch("Preserve pitch", playback.getBoolean("preserve_pitch")) { change("playback.preserve_pitch", it) }
        PreferenceChoice(translate("Volume scale"), playback.getString("volume_scale"),
            listOf("Perceptual" to translate("Perceptual"), "Linear" to translate("Linear")),
            select = { change("playback.volume_scale", it) })
        PreferenceSwitch("Clearing queue also clears the current song", state.getBoolean("clear_queue_includes_current")) { change("clear_queue_includes_current", it) }
    }
    SettingsGroup {
        PreferenceSwitch("Show lyrics", state.getBoolean("fullscreen_lyrics_visible")) { change("fullscreen_lyrics_visible", it) }
        PreferenceSwitch("Show visualizer", state.getBoolean("fullscreen_visualizer_visible")) { change("fullscreen_visualizer_visible", it) }
        PreferenceSwitch("Show current lyrics line", state.getBoolean("fullscreen_current_lyrics_line_visible")) { change("fullscreen_current_lyrics_line_visible", it) }
        PreferenceSwitch("Combine lyrics and visualizer", state.getBoolean("right_panel_combined")) { change("right_panel_combined", it) }
        PreferenceSwitch("Dynamic background", state.getBoolean("fullscreen_dynamic_background")) { change("fullscreen_dynamic_background", it) }
        PreferenceSwitch("Enable background image", state.getBoolean("fullscreen_background_image")) { change("fullscreen_background_image", it) }
        PreferenceSwitch("Waveform seekbar", state.getBoolean("seekbar_waveform_enabled")) { change("seekbar_waveform_enabled", it) }
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun EqualizerSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val equalizer = remember(settings.equalizer) { JSONObject(settings.equalizer) }
    val selected = settings.equalizerPreset
    var resetPreset by remember { mutableStateOf("Flat") }
    var presetsOpen by remember { mutableStateOf(false) }
    LaunchedEffect(selected) { if (selected != "Custom") resetPreset = selected }
    SettingsGroup {
        PreferenceSwitch("Equalizer", equalizer.getBoolean("enabled"), player::equalizerEnabled)
        Box {
            SettingsRow(translate("Preset"), modifier = Modifier.clickable { presetsOpen = true }, trailing = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(settings.equalizerPresets.find { it.id == selected }?.title ?: selected,
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    PlayerIconButton("rufin-edit-undo-symbolic", "Reset", { player.equalizerPreset(resetPreset) }, enabled = selected == "Custom")
                }
            })
            DropdownMenu(presetsOpen, { presetsOpen = false }, Modifier.heightIn(max = 420.dp)) {
                settings.equalizerPresets.forEach { preset ->
                    DropdownMenuItem(text = { Text(preset.title) }, onClick = {
                        player.equalizerPreset(preset.id)
                        presetsOpen = false
                    }, trailingIcon = { RadioButton(selected == preset.id, null) })
                }
            }
        }
    }
    val bands = equalizer.getJSONArray("bands")
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        settings.bandTitles.forEachIndexed { index, title ->
            key(index) {
                val storedValue = bands.getDouble(index).toFloat()
                var value by remember { mutableFloatStateOf(storedValue) }
                LaunchedEffect(storedValue) { value = storedValue }
                var text by remember(storedValue) { mutableStateOf(storedValue.toString()) }
                var editing by remember { mutableStateOf(false) }
                val focus = LocalFocusManager.current
                val parsed = text.replace(',', '.').toFloatOrNull()
                val valid = parsed != null && parsed in -12f..12f
                fun commit() { if (valid) { value = parsed!!; player.equalizerBand(index, value) }; editing = false }
                Surface(Modifier.fillMaxWidth(), RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainer) {
                Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(title, Modifier.width(44.dp), style = MaterialTheme.typography.labelMedium)
                    Slider(value, { value = round(it * 10) / 10; text = value.toString() },
                        modifier = Modifier.weight(1f).semantics { contentDescription = title },
                        colors = SliderDefaults.colors(inactiveTrackColor = MaterialTheme.colorScheme.onSurface.copy(alpha = .24f)),
                        valueRange = -12f..12f,
                        onValueChangeFinished = { player.equalizerBand(index, value) })
                    OutlinedTextField(text, { text = it; editing = true }, Modifier.width(96.dp)
                        .onFocusChanged { if (!it.isFocused && editing) commit() }, singleLine = true, colors = settingsTextFieldColors(),
                        isError = editing && !valid, suffix = { Text("dB", style = MaterialTheme.typography.labelSmall) },
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done),
                        keyboardActions = KeyboardActions(onDone = { if (valid) { commit(); focus.clearFocus() } }))
                }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LyricsSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val lyrics = remember(settings.lyrics) { JSONObject(settings.lyrics) }
    var highlightEdit by remember { mutableStateOf(false) }
    val accent = MaterialTheme.colorScheme.primary
    fun change(key: String, value: Any?) { player.lyricsPreference(key, value) }
    SettingsGroup(translate("Sources")) {
    PreferenceSwitch("External lyrics lookup", lyrics.getBoolean("external_lyrics_enabled")) { change("external_lyrics_enabled", it) }
    PreferenceSwitch("Prefer server lyrics", lyrics.getBoolean("prefer_server_lyrics")) { change("prefer_server_lyrics", it) }
    }
    SettingsGroup(translate("Lyrics storage")) {
    PreferenceSwitch("Saves lyrics to your source", lyrics.getBoolean("save_lyrics_to_source")) { change("save_lyrics_to_source", it) }
    PreferenceSwitch("Save lyrics automatically", lyrics.getBoolean("save_lyrics_automatically")) { change("save_lyrics_automatically", it) }
    val providers = lyrics.getJSONArray("external_lyrics_providers")
    val order = (0 until providers.length()).map { providers.getString(it) }
    SettingsReorderRows(settings.lyricsProviders.sortedBy { order.indexOf(it.id).let { index -> if (index < 0) Int.MAX_VALUE else index } }
        .map { SettingsReorderItem(it.id, it.title, it.id in order, it.id in order) },
        onReorder = { next, _, _, _ -> change("external_lyrics_providers", JSONArray(next.filter { it in order })) },
        onToggle = { id, enabled -> change("external_lyrics_providers", JSONArray(if (enabled) order + id else order - id)) })
    PreferenceChoice(translate("Lyrics storage"), if (lyrics.getBoolean("save_lyrics_as_sidecar")) "sidecar" else "embedded",
        listOf("sidecar" to translate("Save in folder"), "embedded" to translate("Embed in track")),
        select = { change("save_lyrics_as_sidecar", it == "sidecar") })
    }
    SettingsGroup(translate("Language and readings")) {
    PreferenceSwitch("Prefer translations", lyrics.getBoolean("prefer_translations")) { change("prefer_translations", it) }
    PreferenceText("Translation language", lyrics.getString("preferred_translation_language")) { change("preferred_translation_language", it) }
    PreferenceSwitch("Furigana", lyrics.getBoolean("show_furigana")) { change("show_furigana", it) }
    PreferenceSwitch("Romaji", lyrics.getBoolean("show_romanization")) { change("show_romanization", it) }
    }
    player.dictionaryState?.let { state ->
        when (state) {
            "loading", "downloading" -> Text(translate("Downloading Japanese dictionary..."), style = MaterialTheme.typography.bodySmall)
            "failed" -> TextButton(player::prepareDictionary) { Text(translate("Retry")) }
        }
    }
    SettingsGroup(translate("Karaoke Playback")) {
    PreferenceSwitch("Karaoke mode", lyrics.getBoolean("karaoke_mode")) { change("karaoke_mode", it) }
    val highlightColor = if (lyrics.isNull("lyrics_highlight_color")) accent else playerLyricsColor(lyrics.getString("lyrics_highlight_color"))
    ColorSetting("Karaoke highlight color", highlightColor) { highlightEdit = true }
    }
    SettingsGroup(translate("Typography")) {
    PreferenceText("Font family", if (lyrics.isNull("fullscreen_lyrics_font_family")) "" else lyrics.getString("fullscreen_lyrics_font_family")) {
        change("fullscreen_lyrics_font_family", it.ifBlank { null })
    }
    PreferenceSlider("Font size (px)", if (lyrics.isNull("fullscreen_lyrics_font_size")) 19f else lyrics.getInt("fullscreen_lyrics_font_size").toFloat(), 12f..28f, 15) {
        change("fullscreen_lyrics_font_size", it.toInt())
    }
    }
    SettingsGroup(translate("Sidebar")) {
    PreferenceText("Font family", if (lyrics.isNull("lyrics_font_family")) "" else lyrics.getString("lyrics_font_family")) { change("lyrics_font_family", it.ifBlank { null }) }
    PreferenceSlider("Font size (px)", if (lyrics.isNull("lyrics_font_size")) 19f else lyrics.getInt("lyrics_font_size").toFloat(), 12f..28f, 15) { change("lyrics_font_size", it.toInt()) }
    }
    if (highlightEdit) PlayerColorDialog("Karaoke highlight color",
        if (lyrics.isNull("lyrics_highlight_color")) accent else playerLyricsColor(lyrics.getString("lyrics_highlight_color")),
        { highlightEdit = false }) { color ->
        change("lyrics_highlight_color", String.format(Locale.ROOT, "#%02x%02x%02x", (color.red * 255).toInt(), (color.green * 255).toInt(), (color.blue * 255).toInt()))
        highlightEdit = false
    }
}

internal fun playerLyricsColor(value: String): Color {
    val red = value.substring(1, 3).toInt(16) / 255f
    val green = value.substring(3, 5).toInt(16) / 255f
    val blue = value.substring(5, 7).toInt(16) / 255f
    val alpha = if (value.length == 9) value.substring(7, 9).toInt(16) / 255f else 1f
    return Color(red, green, blue, alpha)
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun VisualizerSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val visualizer = remember(settings.visualizer) { JSONObject(settings.visualizer) }
    val appearance = visualizer.getJSONObject("appearance")
    val context = LocalContext.current
    fun change(key: String, value: Any?) {
        player.visualizerAppearance(key, value)
    }
    val slot = settings.visualizerPreset.toInt()
    var colorEdit by remember { mutableStateOf<Int?>(null) }
    SettingsGroup {
        PreferenceChoice(translate("Preset"), slot.toString(), (0 until 5).map { it.toString() to (translate("Preset") + " " + (it + 1)) },
            select = { player.visualizerPreset(it.toUInt(), false) })
    }
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.End) {
        PlayerIconButton("rufin-document-save-symbolic", "Save", { player.visualizerPreset(slot.toUInt(), true) })
        PlayerIconButton("rufin-edit-copy-symbolic", "Copy preset", {
            val text = player.bridge?.copyVisualizerPreset() ?: return@PlayerIconButton
            (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText(translate("Visualizer"), text))
        })
        PlayerIconButton("rufin-document-open-symbolic", "Paste preset", {
            val clip = (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).primaryClip
            clip?.getItemAt(0)?.coerceToText(context)?.toString()?.let(player::pasteVisualizer)
        })
    }
    SettingsGroup(translate("Appearance")) {
    PreferenceChoice(translate("Style"), appearance.getString("style"), settings.visualizerStyles.map { it.id to it.title },
        select = { change("style", it) })
    val accent = MaterialTheme.colorScheme.primary
    PreferenceSwitch("Use accent color", appearance.isNull("colors")) { enabled ->
        if (enabled) change("colors", null)
        else {
            player.bridge?.defaultVisualizerColors(io.github.screwys.rufin.core.AndroidRgb(accent.red, accent.green, accent.blue))?.let { colors ->
                change("colors", JSONArray(colors.map { listOf(it.red, it.green, it.blue) }))
            }
        }
    }
    if (!appearance.isNull("colors")) {
        val colors = appearance.getJSONArray("colors")
        for (index in 0..1) {
            val color = colors.getJSONArray(index)
            ColorSetting(if (index == 0) "Gradient start" else "Gradient end",
                Color(color.getDouble(0).toFloat(), color.getDouble(1).toFloat(), color.getDouble(2).toFloat())) { colorEdit = index }
        }
    }
    PreferenceSlider("Bar spacing (px)", appearance.getDouble("spacing").toFloat(), 0f..12f, 23) { change("spacing", it.toDouble()) }
    PreferenceSlider("Opacity (%)", (appearance.getDouble("opacity") * 100).toFloat(), 0f..100f, 99) { change("opacity", it / 100.0) }
    PreferenceSlider("Frame rate limit", visualizer.getInt("fps_limit").toFloat(), 1f..180f, 178) {
        player.visualizerFrameLimit(it.toInt())
    }
    }
    SettingsGroup(translate("Motion")) {
    PreferenceSlider("Rise speed (%)", (appearance.getDouble("rise") * 100).toFloat(), 1f..100f, 98) { change("rise", it / 100.0) }
    PreferenceSlider("Fall speed (%)", (appearance.getDouble("fall") * 100).toFloat(), 1f..100f, 98) { change("fall", it / 100.0) }
    PreferenceSwitch("Show peak markers", appearance.getBoolean("peaks")) { change("peaks", it) }
    PreferenceSlider("Peak hold (seconds)", appearance.getDouble("peak_hold").toFloat(), 0f..5f, 49) { change("peak_hold", it.toDouble()) }
    PreferenceSlider("Peak fall speed (% per second)", (appearance.getDouble("peak_fall") * 100).toFloat(), 1f..200f, 198) { change("peak_fall", it / 100.0) }
    }
    SettingsGroup(translate("Lyrics background opacity")) {
        PreferenceSlider("Fullscreen", (appearance.getDouble("fullscreen_lyrics_opacity") * 100).toFloat(), 0f..100f, 99) { change("fullscreen_lyrics_opacity", it / 100.0) }
        PreferenceSlider("Sidebar", (appearance.getDouble("sidebar_lyrics_opacity") * 100).toFloat(), 0f..100f, 99) { change("sidebar_lyrics_opacity", it / 100.0) }
    }
    SettingsGroup {
        SettingsRow(translate("Reset appearance"),
            leading = { RufinIcon("rufin-edit-undo-symbolic", null, Modifier.size(24.dp)) },
            modifier = Modifier.clickable(onClick = player::resetVisualizer))
    }
    colorEdit?.let { endpoint ->
        val color = appearance.getJSONArray("colors").getJSONArray(endpoint)
        PlayerColorDialog(if (endpoint == 0) "Gradient start" else "Gradient end", Color(color.getDouble(0).toFloat(), color.getDouble(1).toFloat(), color.getDouble(2).toFloat()),
            { colorEdit = null }) { selected ->
            val colors = JSONArray(appearance.getJSONArray("colors").toString())
            colors.put(endpoint, JSONArray(listOf(selected.red, selected.green, selected.blue)))
            change("colors", colors); colorEdit = null
        }
    }
}

@Composable
private fun ColorSetting(label: String, color: Color, edit: () -> Unit) {
    SettingsRow(translate(label),
        leading = { Surface(Modifier.size(28.dp), RoundedCornerShape(8.dp), color = color) {} },
        modifier = Modifier.clickable(onClick = edit))
}

@Composable
private fun PreferenceSwitch(label: String, checked: Boolean, change: (Boolean) -> Unit) {
    SettingsSwitch(translate(label), checked, change = change)
}

@Composable
private fun PreferenceSlider(label: String, value: Float, range: ClosedFloatingPointRange<Float>, steps: Int, change: (Float) -> Unit) {
    SettingsSliderNumber(translate(label), value.toString().toDouble(), range.start.toDouble()..range.endInclusive.toDouble(),
        (range.endInclusive - range.start).toDouble() / (steps + 1)) { change((it as Number).toFloat()) }
}

@Composable
private fun PreferenceText(label: String, value: String, change: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    var draft by remember(value) { mutableStateOf(value) }
    SettingsRow(translate(label),
        summary = value.ifBlank { translate("System default") },
        modifier = Modifier.clickable { draft = value; open = true })
    if (open) AlertDialog(onDismissRequest = { open = false }, title = { Text(translate(label)) },
        text = { OutlinedTextField(draft, { draft = it }, modifier = Modifier.fillMaxWidth(), singleLine = true, colors = settingsTextFieldColors()) },
        confirmButton = { TextButton({ change(draft); open = false }) { Text(translate("Save")) } },
        dismissButton = { TextButton({ open = false }) { Text(translate("Cancel")) } })
}

@Composable
private fun PlayerColorDialog(label: String, initial: Color, onDismiss: () -> Unit, save: (Color) -> Unit) {
    var red by remember { mutableFloatStateOf(initial.red) }
    var green by remember { mutableFloatStateOf(initial.green) }
    var blue by remember { mutableFloatStateOf(initial.blue) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate(label)) }, text = {
        Column {
            Surface(Modifier.fillMaxWidth().height(64.dp), color = Color(red, green, blue)) {}
            PreferenceSlider("Red", red * 255, 0f..255f, 254) { red = it / 255 }
            PreferenceSlider("Green", green * 255, 0f..255f, 254) { green = it / 255 }
            PreferenceSlider("Blue", blue * 255, 0f..255f, 254) { blue = it / 255 }
        }
    }, confirmButton = { TextButton({ save(Color(red, green, blue)) }) { Text(translate("Save")) } },
        dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}
