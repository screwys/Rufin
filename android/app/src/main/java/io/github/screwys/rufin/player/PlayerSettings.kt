package io.github.screwys.rufin.player

import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
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
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.core.tween
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.shared.playback.PlaybackTransitionMode
import io.github.screwys.rufin.shared.playback.LoudnessNormalization
import io.github.screwys.rufin.shared.playback.LoudnessNormalizationScope
import io.github.screwys.rufin.shared.playback.VolumeScale
import io.github.screwys.rufin.shared.playback.StreamQuality
import io.github.screwys.rufin.shared.lyrics.ExternalLyricsProvider
import io.github.screwys.rufin.shared.rufin_core.VisualizerStyle
import io.github.screwys.rufin.settings.SettingsNumber
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.lyrics.JapaneseDictionaryStatus
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import kotlinx.coroutines.launch
import java.util.Locale

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
            PlayerSettingsPage.PREFERENCES -> PlaybackPreferences(player, preferences)
        }
    }
}

@Composable
internal fun PlaybackPreferences(player: PlayerConnection, preferences: PreferencesConnection?, output: (() -> Unit)? = null, includeDisplaySettings: Boolean = true) {
    val state = player.settings ?: return
    val options = player.preferenceOptions ?: return
    val playback = state.playback
    val transitionDuration = if (LocalReduceMotion.current) 0 else 180
    var confirmEbu by remember { mutableStateOf(false) }
    fun change(edit: SettingsOwner.() -> Unit) { player.editPreferences(edit) }
    SettingsGroup {
        PreferenceChoice(translate("Transition mode"), playback.transitionMode.name,
            listOf(PlaybackTransitionMode.GAPLESS.name to translate("Gapless"), PlaybackTransitionMode.CROSSFADE.name to translate("Crossfade")),
            select = { change { setTransitionMode(PlaybackTransitionMode.valueOf(it)) } })
        AnimatedVisibility(playback.transitionMode == PlaybackTransitionMode.CROSSFADE,
            enter = expandVertically(tween(transitionDuration)) + fadeIn(tween(transitionDuration)),
            exit = shrinkVertically(tween(transitionDuration)) + fadeOut(tween(transitionDuration))) {
            Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                SettingsSliderNumber(translate("Crossfade duration"), playback.crossfadeSeconds.toDouble(),
                    options.minCrossfadeSeconds.toDouble()..options.maxCrossfadeSeconds.toDouble(), 1.0, integer = true, unit = "s") {
                    change { setCrossfadeSeconds((it as Number).toInt().toUByte()) }
                }
                PreferenceSwitch("Skip same-album crossfade", playback.skipSameAlbumCrossfade) { change { setSkipSameAlbumCrossfade(it) } }
            }
        }
        PreferenceSwitch("Audio fade on play/pause", playback.audioFadeOnStatusChange) { change { setAudioFadeOnStatusChange(it) } }
        SettingsSliderNumber(translate("Playback speed"), playback.playbackRate,
            options.minPlaybackRate..options.maxPlaybackRate, .05, unit = "×") { change { setPlaybackRate((it as Number).toDouble()) } }
        PreferenceSwitch("Preserve pitch", playback.preservePitch) { change { setPreservePitch(it) } }
        PreferenceChoice(translate("Volume scale"), playback.volumeScale.name,
            listOf(VolumeScale.PERCEPTUAL.name to translate("Perceptual"), VolumeScale.LINEAR.name to translate("Linear")),
            select = { change { setVolumeScale(VolumeScale.valueOf(it)) } })
        PreferenceSwitch("Clearing queue also clears the current song", state.display.clearQueueIncludesCurrent) { change { setClearQueueIncludesCurrent(it) } }
        if (output != null) preferences?.applicationSettings?.let { application ->
            SettingsNumber(translate("Auto DJ refill threshold"), application.autoDjRefillThreshold.toDouble(),
                options.minAutoDjRefillThreshold.toDouble()..options.maxAutoDjRefillThreshold.toDouble()) {
                preferences.changePreference { setAutoDjRefillThreshold((it as Number).toInt().toUByte()) }
            }
        }
    }
    if (output != null) SettingsGroup(translate("Audio")) {
        SettingsRow(translate("Audio output"), modifier = Modifier.clickable(onClick = output))
        PreferenceChoice(translate("Loudness normalization"), playback.loudnessNormalization.name,
            listOf(LoudnessNormalization.OFF.name to translate("Off"), LoudnessNormalization.REPLAY_GAIN.name to "ReplayGain", LoudnessNormalization.EBU_R128.name to "EBU R128"), select = {
                val mode = LoudnessNormalization.valueOf(it)
                if (mode == LoudnessNormalization.EBU_R128 && playback.loudnessNormalization != mode) confirmEbu = true
                else change { setLoudnessNormalization(mode) }
            })
        PreferenceChoice(translate("Normalization scope"), playback.loudnessNormalizationScope.name,
            listOf(LoudnessNormalizationScope.TRACK.name to translate("Track"), LoudnessNormalizationScope.ALBUM.name to translate("Album")),
            select = { change { setLoudnessNormalizationScope(LoudnessNormalizationScope.valueOf(it)) } })
        SettingsSliderNumber(translate("Target loudness"), playback.ebuR128TargetLufs,
            options.minEbuR128TargetLufs..options.maxEbuR128TargetLufs, .1, unit = "LUFS") { change { setEbuR128TargetLufs((it as Number).toDouble()) } }
        PreferenceSwitch("Write EBU R128 tags to files", playback.writeEbuR128Tags) { change { setWriteEbuR128Tags(it) } }
        val bitrate = (playback.streamQuality as? StreamQuality.MaxBitrateKbps)?.v1?.toString() ?: "original"
        PreferenceChoice(translate("Stream quality"), bitrate,
            listOf("original" to translate("Original")) + listOf(320, 256, 192, 128).map { it.toString() to "$it kbps" },
            select = { change { setStreamQuality(if (it == "original") StreamQuality.Original else StreamQuality.MaxBitrateKbps(it.toUInt())) } })
    }
    if (includeDisplaySettings) {
        PlayerLayoutPreferences(player)
        SettingsGroup {
            PreferenceSwitch("Waveform seekbar", state.display.seekbarWaveformEnabled) { change { setSeekbarWaveformEnabled(it) } }
        }
    }
    if (confirmEbu) AlertDialog(onDismissRequest = { confirmEbu = false },
        title = { Text(translate("Enable EBU R128 Analysis?")) },
        text = { Text(translate("Rufin will calculate the missing EBU R128 metadata for the whole library. This can use significant CPU, battery, and network bandwidth for a long time.")) },
        confirmButton = { TextButton({ confirmEbu = false; change { setLoudnessNormalization(LoudnessNormalization.EBU_R128) } }) { Text(translate("Enable")) } },
        dismissButton = { TextButton({ confirmEbu = false }) { Text(translate("Cancel")) } })
}

@Composable
internal fun PlayerLayoutPreferences(player: PlayerConnection) {
    val state = player.settings ?: return
    fun change(edit: SettingsOwner.() -> Unit) { player.editPreferences(edit) }
    SettingsGroup(translate("Layout")) {
        PreferenceSwitch("Show lyrics", state.display.fullscreenLyricsVisible) { change { setFullscreenLyricsVisible(it) } }
        PreferenceSwitch("Show visualizer", state.display.fullscreenVisualizerVisible) { change { setFullscreenVisualizerVisible(it) } }
        PreferenceSwitch("Show current lyrics line", state.display.fullscreenCurrentLyricsLineVisible) { change { setFullscreenCurrentLyricsLineVisible(it) } }
        PreferenceSwitch("Combine lyrics and visualizer", state.rightPanel.combined) { change { setRightPanelCombined(it) } }
        PreferenceSwitch("Dynamic background", state.display.fullscreenDynamicBackground) { change { setFullscreenDynamicBackground(it) } }
        PreferenceSwitch("Enable background image", state.display.fullscreenBackgroundImage) { change { setFullscreenBackgroundImage(it) } }
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun EqualizerSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val equalizer = settings.playback.equalizer
    val options = player.preferenceOptions ?: return
    val selected = equalizer.selectedPreset
    var resetPreset by remember { mutableStateOf("Flat") }
    var presetsOpen by remember { mutableStateOf(false) }
    LaunchedEffect(selected) { if (selected != "Custom") resetPreset = selected }
    SettingsGroup {
        PreferenceSwitch("Equalizer", equalizer.enabled, player::equalizerEnabled)
        Box {
            SettingsRow(translate("Preset"), modifier = Modifier.clickable { presetsOpen = true }, trailing = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(translate(selected),
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    PlayerIconButton("rufin-edit-undo-symbolic", "Reset", { player.equalizerPreset(resetPreset) }, enabled = selected == "Custom")
                }
            })
            DropdownMenu(presetsOpen, { presetsOpen = false }, Modifier.heightIn(max = 420.dp)) {
                options.equalizerPresets.forEach { preset ->
                    DropdownMenuItem(text = { Text(translate(preset)) }, onClick = {
                        player.equalizerPreset(preset)
                        presetsOpen = false
                    }, trailingIcon = { RadioButton(selected == preset, null) })
                }
            }
        }
    }
    val bands = equalizer.bands
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        options.equalizerBandTitles.forEachIndexed { index, title ->
            key(index) {
                SettingsSliderNumber(title, bands[index], -12.0..12.0, .1, unit = "dB") {
                    player.equalizerBand(index, (it as Number).toDouble())
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LyricsSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val lyrics = settings.lyrics
    var highlightEdit by remember { mutableStateOf(false) }
    val accent = MaterialTheme.colorScheme.primary
    fun change(edit: SettingsOwner.() -> Unit) { player.lyricsPreference(edit) }
    SettingsGroup(translate("Sources")) {
    PreferenceSwitch("External lyrics lookup", lyrics.externalLyricsEnabled) { change { setExternalLyricsEnabled(it) } }
    PreferenceSwitch("Prefer server lyrics", lyrics.preferServerLyrics) { change { setPreferServerLyrics(it) } }
    }
    SettingsGroup(translate("Lyrics storage")) {
    PreferenceSwitch("Saves lyrics to your source", lyrics.saveLyricsToSource) { change { setSaveLyricsToSource(it) } }
    PreferenceSwitch("Save lyrics automatically", lyrics.saveLyricsAutomatically) { change { setSaveLyricsAutomatically(it) } }
    val order = lyrics.externalLyricsProviders
    SettingsReorderRows(ExternalLyricsProvider.entries.sortedBy { order.indexOf(it).let { index -> if (index < 0) Int.MAX_VALUE else index } }
        .map { SettingsReorderItem(it.name, when (it) {
            ExternalLyricsProvider.LRCLIB -> "LRCLIB"
            ExternalLyricsProvider.NETEASE -> "NetEase"
            ExternalLyricsProvider.GENIUS -> "Genius"
            ExternalLyricsProvider.SIMP_MUSIC -> "SimpMusic"
        }, it in order, it in order) },
        onReorder = { next, _, _, _ -> change { setExternalLyricsProviders(next.map(ExternalLyricsProvider::valueOf).filter { it in order }) } },
        onToggle = { id, enabled -> val provider = ExternalLyricsProvider.valueOf(id); change { setExternalLyricsProviders(if (enabled) order + provider else order - provider) } })
    PreferenceChoice(translate("Lyrics storage"), if (lyrics.saveLyricsAsSidecar) "sidecar" else "embedded",
        listOf("sidecar" to translate("Save in folder"), "embedded" to translate("Embed in track")),
        select = { change { setSaveLyricsAsSidecar(it == "sidecar") } })
    }
    SettingsGroup(translate("Language and readings")) {
    PreferenceSwitch("Prefer translations", lyrics.preferTranslations) { change { setPreferTranslations(it) } }
    PreferenceText("Translation language", lyrics.preferredTranslationLanguage) { change { setPreferredTranslationLanguage(it) } }
    PreferenceSwitch("Furigana", lyrics.showFurigana) { change { setShowFurigana(it) } }
    PreferenceSwitch("Romaji", lyrics.showRomanization) { change { setShowRomanization(it) } }
    }
    player.dictionaryState?.let { state ->
        when (state) {
            JapaneseDictionaryStatus.Loading, JapaneseDictionaryStatus.Downloading -> Text(translate("Downloading Japanese dictionary..."), style = MaterialTheme.typography.bodySmall)
            JapaneseDictionaryStatus.Failed -> TextButton(player::prepareDictionary) { Text(translate("Retry")) }
            else -> Unit
        }
    }
    SettingsGroup(translate("Karaoke Playback")) {
    PreferenceSwitch("Karaoke mode", lyrics.karaokeMode) { change { setKaraokeMode(it) } }
    val highlightColor = lyrics.lyricsHighlightColor?.let(::playerLyricsColor) ?: accent
    ColorSetting("Karaoke highlight color", highlightColor) { highlightEdit = true }
    }
    SettingsGroup(translate("Typography")) {
    PreferenceText("Font family", lyrics.fullscreenLyricsFontFamily.orEmpty()) {
        change { setFullscreenLyricsFontFamily(it.ifBlank { null }) }
    }
    PreferenceSlider("Font size (px)", (lyrics.fullscreenLyricsFontSize?.toFloat() ?: 19f), 12f..28f, 15) {
        change { setFullscreenLyricsFontSize(it.toInt().toUShort()) }
    }
    }
    SettingsGroup(translate("Sidebar")) {
    PreferenceText("Font family", lyrics.lyricsFontFamily.orEmpty()) { change { setLyricsFontFamily(it.ifBlank { null }) } }
    PreferenceSlider("Font size (px)", (lyrics.lyricsFontSize?.toFloat() ?: 19f), 12f..28f, 15) { change { setLyricsFontSize(it.toInt().toUShort()) } }
    }
    if (highlightEdit) PlayerColorDialog("Karaoke highlight color",
        lyrics.lyricsHighlightColor?.let(::playerLyricsColor) ?: accent,
        { highlightEdit = false }) { color ->
        change { setLyricsHighlightColor(String.format(Locale.ROOT, "#%02x%02x%02x", (color.red * 255).toInt(), (color.green * 255).toInt(), (color.blue * 255).toInt())) }
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
    val visualizer = settings.visualizer
    val appearance = visualizer.appearance
    val context = LocalContext.current
    fun change(edit: SettingsOwner.() -> Unit) { player.visualizerAppearance(edit) }
    val slot = visualizer.selectedPreset?.toInt() ?: 0
    var colorEdit by remember { mutableStateOf<Int?>(null) }
    SettingsGroup {
        PreferenceChoice(translate("Preset"), slot.toString(), (0 until 5).map { it.toString() to (translate("Preset") + " " + (it + 1)) },
            select = { player.visualizerPreset(it.toULong(), false) })
    }
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.End) {
        PlayerIconButton("rufin-document-save-symbolic", "Save", { player.visualizerPreset(slot.toULong(), true) })
        PlayerIconButton("rufin-edit-copy-symbolic", "Copy preset", {
            player.copyVisualizer { text ->
                (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText(translate("Visualizer"), text))
            }
        })
        PlayerIconButton("rufin-document-open-symbolic", "Paste preset", {
            val clip = (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).primaryClip
            clip?.getItemAt(0)?.coerceToText(context)?.toString()?.let(player::pasteVisualizer)
        })
    }
    SettingsGroup(translate("Appearance")) {
    PreferenceChoice(translate("Style"), appearance.style.name, VisualizerStyle.entries.map { it.name to translate(when (it) {
        VisualizerStyle.SEGMENTED -> "Segmented bars"
        VisualizerStyle.SOLID -> "Solid bars"
        VisualizerStyle.ROUNDED -> "Rounded bars"
        VisualizerStyle.OUTLINE -> "Outline bars"
        VisualizerStyle.LINE -> "Spectrum line"
        VisualizerStyle.FILLED -> "Filled spectrum"
        VisualizerStyle.MIRRORED -> "Mirrored bars"
        VisualizerStyle.CIRCULAR -> "Circular spectrum"
    }) },
        select = { change { setVisualizerStyle(VisualizerStyle.valueOf(it)) } })
    PreferenceSwitch("Ignore playback volume", settings.playback.visualizerIgnoreVolume) { change { setVisualizerIgnoreVolume(it) } }
    PreferenceSlider("Bar count", appearance.barCount.toFloat(), 8f..128f, 119) { change { setVisualizerBarCount(it.toUInt()) } }
    PreferenceSwitch("Mirrored", appearance.mirrored) { change { setVisualizerMirrored(it) } }
    PreferenceSwitch("Shadow", appearance.shadow) { change { setVisualizerShadow(it) } }
    if (appearance.shadow) {
        PreferenceSlider("Height (%)", (appearance.shadowRatio * 100).toFloat(), 0f..95f, 94) { change { setVisualizerShadowRatio(it / 100.0) } }
        PreferenceSlider("Opacity (%)", (appearance.shadowOpacity * 100).toFloat(), 0f..100f, 99) { change { setVisualizerShadowOpacity(it / 100.0) } }
        PreferenceSlider("Brightness (%)", (appearance.shadowBrightness * 100).toFloat(), 0f..200f, 199) { change { setVisualizerShadowBrightness(it / 100.0) } }
        PreferenceSwitch("Fit to height", appearance.shadowFit) { change { setVisualizerShadowFit(it) } }
    }
    val accent = MaterialTheme.colorScheme.primary
    PreferenceSwitch("Use accent color", (appearance.colors == null)) { enabled ->
        if (enabled) change { setVisualizerColors(null) }
        else {
            player.bridge?.defaultVisualizerColors(io.github.screwys.rufin.core.AndroidRgb(accent.red, accent.green, accent.blue))?.let { colors ->
                change { setVisualizerColors(colors.map { listOf(it.red, it.green, it.blue) }) }
            }
        }
    }
    if (appearance.colors != null) {
        val colors = appearance.colors!!
        for (index in 0..1) {
            val color = colors[index]
            ColorSetting(if (index == 0) "Gradient start" else "Gradient end",
                Color(color[0], color[1], color[2])) { colorEdit = index }
        }
    }
    PreferenceSlider("Bar spacing (px)", appearance.spacing.toFloat(), 0f..12f, 23) { change { setVisualizerSpacing(it.toDouble()) } }
    PreferenceSlider("Opacity (%)", (appearance.opacity * 100).toFloat(), 0f..100f, 99) { change { setVisualizerOpacity(it / 100.0) } }
    PreferenceSlider("Frame rate limit", visualizer.fpsLimit.toFloat(), 1f..180f, 178) {
        player.visualizerFrameLimit(it.toInt())
    }
    }
    SettingsGroup(translate("Motion")) {
    PreferenceSlider("Rise speed (%)", (appearance.rise * 100).toFloat(), 1f..100f, 98) { change { setVisualizerRise(it / 100.0) } }
    PreferenceSlider("Fall speed (%)", (appearance.fall * 100).toFloat(), 1f..100f, 98) { change { setVisualizerFall(it / 100.0) } }
    PreferenceSwitch("Show peak markers", appearance.peaks) { change { setVisualizerPeaks(it) } }
    PreferenceSlider("Peak hold (seconds)", appearance.peakHold.toFloat(), 0f..5f, 49) { change { setVisualizerPeakHold(it.toDouble()) } }
    PreferenceSlider("Peak fall speed (% per second)", (appearance.peakFall * 100).toFloat(), 1f..200f, 198) { change { setVisualizerPeakFall(it / 100.0) } }
    }
    SettingsGroup(translate("Lyrics background opacity")) {
        PreferenceSlider("Fullscreen", (appearance.fullscreenLyricsOpacity * 100).toFloat(), 0f..100f, 99) { change { setVisualizerFullscreenLyricsOpacity(it / 100.0) } }
        PreferenceSlider("Sidebar", (appearance.sidebarLyricsOpacity * 100).toFloat(), 0f..100f, 99) { change { setVisualizerSidebarLyricsOpacity(it / 100.0) } }
    }
    SettingsGroup {
        SettingsRow(translate("Reset appearance"),
            leading = { RufinIcon("rufin-edit-undo-symbolic", null, Modifier.size(24.dp)) },
            modifier = Modifier.clickable(onClick = player::resetVisualizer))
    }
    colorEdit?.let { endpoint ->
        val color = appearance.colors!![endpoint]
        PlayerColorDialog(if (endpoint == 0) "Gradient start" else "Gradient end", Color(color[0], color[1], color[2]),
            { colorEdit = null }) { selected ->
            val colors = appearance.colors!!.toMutableList()
            colors[endpoint] = listOf(selected.red, selected.green, selected.blue)
            change { setVisualizerColors(colors) }; colorEdit = null
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
