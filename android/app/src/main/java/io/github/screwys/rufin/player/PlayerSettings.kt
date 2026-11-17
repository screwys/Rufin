package io.github.screwys.rufin.player


import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.shared.rufin_core.SourceSummary
import io.github.screwys.rufin.shared.rufin_core.showsServerRecommendations
import io.github.screwys.rufin.shared.library.SameArtist
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SegmentedPill
import io.github.screwys.rufin.ui.PagerTabIndicator
import io.github.screwys.rufin.ui.showPage
import androidx.activity.compose.BackHandler
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.SettingsSwitch
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.SettingsSliderNumber
import io.github.screwys.rufin.settings.SettingsText

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
import androidx.compose.animation.core.Animatable
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.*
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.shared.playback.PlaybackTransitionMode
import io.github.screwys.rufin.shared.playback.LoudnessNormalization
import io.github.screwys.rufin.shared.playback.LoudnessNormalizationScope
import io.github.screwys.rufin.shared.playback.VolumeScale
import io.github.screwys.rufin.downloads.AudioQualityControls
import io.github.screwys.rufin.shared.rufin_core.VisualizerStyle
import io.github.screwys.rufin.shared.rufin_core.VisualizerFrequencyScale
import io.github.screwys.rufin.shared.rufin_core.VisualizerWeighting
import io.github.screwys.rufin.settings.SettingsNumber
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.lyrics.JapaneseDictionaryStatus
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import kotlinx.coroutines.launch
import kotlinx.coroutines.CancellationException
import java.util.Locale

internal enum class PlayerSettingsPage(val title: String) {
    PLAYBACK("Playback"), EQUALIZER("Equalizer"), LYRICS("Lyrics"), VISUALIZER("Visualizer")
}

/** The player's settings page: GTK's player settings, equalizer and lyrics/visualizer settings as tabs. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlayerSettingsScreen(player: PlayerConnection, onClose: () -> Unit) {
    val pager = rememberPagerState { PlayerSettingsPage.entries.size }
    var visited by remember { mutableStateOf(setOf(0)) }
    val settledPage by remember { derivedStateOf { pager.settledPage } }
    val selectedPage by remember { derivedStateOf { pager.targetPage } }
    LaunchedEffect(settledPage) { visited = visited + settledPage }
    val fade = remember { Animatable(1f) }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    BackHandler(onBack = onClose)
    Surface(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClose) { RufinIcon("rufin-go-previous-symbolic", translate("Back")) }
                Text(translate("Settings"), Modifier.weight(1f).padding(start = 8.dp), style = MaterialTheme.typography.titleLarge)
            }
            PrimaryTabRow(selectedPage, containerColor = Color.Transparent, indicator = { PagerTabIndicator(pager) }) {
                PlayerSettingsPage.entries.forEachIndexed { index, page ->
                    Tab(selectedPage == index, { scope.launch { pager.showPage(index, fade, reduceMotion) } },
                        text = { Text(translate(page.title), maxLines = 1) })
                }
            }
            HorizontalPager(pager, Modifier.fillMaxWidth().weight(1f).graphicsLayer { alpha = fade.value }, verticalAlignment = Alignment.Top,
                beyondViewportPageCount = PlayerSettingsPage.entries.size - 1) { index ->
                Box(Modifier.fillMaxSize()) {
                    if (index in visited) CompositionLocalProvider(LocalPlayerBottomPadding provides 0.dp) {
                        PlayerSettingsContent(player, PlayerSettingsPage.entries[index], active = index == settledPage)
                    }
                }
            }
        }
    }
}

@Composable
internal fun PlayerSettingsContent(player: PlayerConnection, page: PlayerSettingsPage, active: Boolean = true) {
    SettingsPage {
        if (player.settings == null) Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        else when (page) {
            PlayerSettingsPage.PLAYBACK -> PlayerPlaybackSettings(player)
            PlayerSettingsPage.EQUALIZER -> EqualizerSettings(player, active)
            PlayerSettingsPage.LYRICS -> LyricsSettings(player)
            PlayerSettingsPage.VISUALIZER -> VisualizerSettings(player)
        }
    }
}

/** As GTK's player settings popover. */
@Composable
private fun PlayerPlaybackSettings(player: PlayerConnection) {
    SettingsGroup { TransitionAndSpeed(player) }
}

/** Transition, crossfade, speed and pitch: the first rows of both GTK's playback page and its player settings. */
@Composable
private fun TransitionAndSpeed(player: PlayerConnection) {
    val playback = player.settings?.playback ?: return
    val options = player.preferenceOptions ?: return
    val transitionDuration = if (LocalReduceMotion.current) 0 else 180
    fun change(edit: SettingsOwner.() -> Unit) { player.editPreferences(edit) }
    PreferenceChoice(translate("Transition mode"), playback.transitionMode.name,
        listOf(PlaybackTransitionMode.GAPLESS.name to translate("Gapless"), PlaybackTransitionMode.CROSSFADE.name to translate("Crossfade")),
        select = { change { setTransitionMode(PlaybackTransitionMode.valueOf(it)) } })
    AnimatedVisibility(playback.transitionMode == PlaybackTransitionMode.CROSSFADE,
        enter = expandVertically(tween(transitionDuration)) + fadeIn(tween(transitionDuration)),
        exit = shrinkVertically(tween(transitionDuration)) + fadeOut(tween(transitionDuration))) {
        Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
            SettingsSliderNumber(translate("Crossfade duration"), playback.crossfadeSeconds.toDouble(),
                options.minCrossfadeSeconds.toDouble()..options.maxCrossfadeSeconds.toDouble(), 1.0, unit = "s",
                default = options.defaultCrossfadeSeconds.toDouble(), marks = options.crossfadeMarks) {
                change { setCrossfadeSeconds(it.toInt().toUByte()) }
            }
            PreferenceSwitch("Skip same-album crossfade", playback.skipSameAlbumCrossfade) { change { setSkipSameAlbumCrossfade(it) } }
        }
    }
    SettingsSliderNumber(translate("Playback speed"), playback.playbackRate, options.minPlaybackRate..options.maxPlaybackRate, .05,
        unit = "×", default = options.defaultPlaybackRate, marks = options.playbackRateMarks) { change { setPlaybackRate(it) } }
    PreferenceSwitch("Preserve pitch", playback.preservePitch) { change { setPreservePitch(it) } }
}

private enum class PlaybackTab(val title: String) { Queue("Queue"), AutoDj("Auto DJ"), Audio("Audio") }

@Composable
internal fun PlaybackPreferences(player: PlayerConnection, preferences: PreferencesConnection, sources: List<SourceSummary>, output: () -> Unit) {
    val state = player.settings ?: return
    val options = player.preferenceOptions ?: return
    val playback = state.playback
    var confirmEbu by remember { mutableStateOf(false) }
    var tab by rememberSaveable { mutableStateOf(PlaybackTab.Queue) }
    fun change(edit: SettingsOwner.() -> Unit) { player.editPreferences(edit) }
    Column(Modifier.fillMaxSize()) {
        SegmentedPill(PlaybackTab.entries, tab, { tab = it }, Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            equal = true) { option, _ ->
            Text(translate(option.title), style = MaterialTheme.typography.titleSmall, maxLines = 1)
        }
        SettingsPage(Modifier.weight(1f)) {
            when (tab) {
                PlaybackTab.Queue -> {
                    SettingsGroup {
                        TransitionAndSpeed(player)
                        PreferenceSwitch("Audio fade on play/pause", playback.audioFadeOnStatusChange) { change { setAudioFadeOnStatusChange(it) } }
                        PreferenceSwitch("Skip unplayable tracks", playback.skipUnplayableTracks) { change { setSkipUnplayableTracks(it) } }
                        PreferenceSwitch("Clearing queue also clears the current song", state.display.clearQueueIncludesCurrent) { change { setClearQueueIncludesCurrent(it) } }
                    }
                }
                PlaybackTab.AutoDj -> {
                    preferences.applicationSettings?.let { application ->
                        SettingsGroup(description = translate("What Auto DJ and radio pick")) {
                            // The choice shows where a configured server recommends; unset reads as on.
                            if (showsServerRecommendations(sources)) SettingsSwitch(translate("Override server recommendations"), application.overrideServerRecommendations != false,
                                summary = translate("Play Rufin's picks instead of the server's own")) { preferences.changePreference { setOverrideServerRecommendations(it) } }
                            SettingsSwitch(translate("Similar artists from ListenBrainz"), application.autoDjSimilarArtists,
                                summary = translate("Look up the artists that listeners play alongside the current one")) { preferences.changePreference { setAutoDjSimilarArtists(it) } }
                            SettingsNumber(translate("Auto DJ refill threshold"), application.autoDjRefillThreshold.toDouble(),
                                options.minAutoDjRefillThreshold.toDouble()..options.maxAutoDjRefillThreshold.toDouble()) {
                                preferences.changePreference { setAutoDjRefillThreshold((it as Number).toInt().toUByte()) }
                            }
                            SettingsNumber(translate("Tracks per refill"), application.autoDjRefillCount.toDouble(),
                                options.minAutoDjRefillCount.toDouble()..options.maxAutoDjRefillCount.toDouble()) {
                                preferences.changePreference { setAutoDjRefillCount((it as Number).toInt().toUByte()) }
                            }
                            PreferenceChoice(translate("Same artist"), application.autoDjSameArtist.name,
                                listOf(SameArtist.FEW.name to translate("Few"), SameArtist.SOME.name to translate("Some"), SameArtist.MANY.name to translate("Many")),
                                select = { preferences.changePreference { setAutoDjSameArtist(SameArtist.valueOf(it)) } })
                            SettingsSwitch(translate("Skip low-rated tracks"), application.autoDjSkipLowRated,
                                summary = translate("Leave out tracks rated two stars or lower")) { preferences.changePreference { setAutoDjSkipLowRated(it) } }
                        }
                    }
                }
                PlaybackTab.Audio -> {
                    SettingsGroup {
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
                            options.minEbuR128TargetLufs..options.maxEbuR128TargetLufs, .1, unit = "LUFS",
                            default = options.defaultEbuR128TargetLufs, marks = options.ebuR128TargetMarks) { change { setEbuR128TargetLufs(it) } }
                        PreferenceSwitch("Write EBU R128 tags to files", playback.writeEbuR128Tags) { change { setWriteEbuR128Tags(it) } }
                        PreferenceChoice(translate("Volume scale"), playback.volumeScale.name,
                            listOf(VolumeScale.PERCEPTUAL.name to translate("Perceptual"), VolumeScale.LINEAR.name to translate("Linear")),
                            select = { change { setVolumeScale(VolumeScale.valueOf(it)) } })
                    }
                    SettingsGroup(translate("Stream quality")) {
                        AudioQualityControls(playback.streamQuality) { quality -> quality?.let { change { setStreamQuality(it) } } }
                    }
                    preferences.applicationSettings?.let { application ->
                        SettingsGroup(translate("Stream quality on mobile data")) {
                            AudioQualityControls(application.mobileDataStreamQuality, follows = true) { quality ->
                                preferences.changePreference { setMobileDataStreamQuality(quality) }
                            }
                        }
                    }
                }
            }
        }
    }
    if (confirmEbu) AlertDialog(onDismissRequest = { confirmEbu = false },
        title = { Text(translate("Enable EBU R128 Analysis?")) },
        text = { Text(translate("Rufin will calculate the missing EBU R128 metadata for the whole library. This can use significant CPU, battery, and network bandwidth for a long time.")) },
        confirmButton = { TextButton({ confirmEbu = false; change { setLoudnessNormalization(LoudnessNormalization.EBU_R128) } }) { Text(translate("Enable")) } },
        dismissButton = { TextButton({ confirmEbu = false }) { Text(translate("Cancel")) } })
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LyricsSettings(player: PlayerConnection) {
    val settings = player.settings ?: return
    val lyrics = settings.lyrics
    var highlightEdit by remember { mutableStateOf(false) }
    val accent = MaterialTheme.colorScheme.primary
    fun change(edit: SettingsOwner.() -> Unit) { player.lyricsPreference(edit) }
    SettingsGroup(translate("Lyrics storage")) {
    PreferenceSwitch("Saves lyrics to your source", lyrics.saveLyricsToSource) { change { setSaveLyricsToSource(it) } }
    PreferenceSwitch("Save lyrics automatically", lyrics.saveLyricsAutomatically) { change { setSaveLyricsAutomatically(it) } }
    PreferenceChoice(translate("Lyrics storage"), if (lyrics.saveLyricsAsSidecar) "sidecar" else "embedded",
        listOf("sidecar" to translate("Save in folder"), "embedded" to translate("Embed in track")),
        select = { change { setSaveLyricsAsSidecar(it == "sidecar") } })
    }
    SettingsGroup(translate("Language and readings")) {
    PreferenceSwitch("Prefer translations", lyrics.preferTranslations) { change { setPreferTranslations(it) } }
    SettingsText(translate("Translation language"), lyrics.preferredTranslationLanguage, lyrics.preferredTranslationLanguage.ifBlank { translate("System default") }) { change { setPreferredTranslationLanguage(it) } }
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
    SettingsText(translate("Font family"), lyrics.fullscreenLyricsFontFamily.orEmpty(), lyrics.fullscreenLyricsFontFamily.orEmpty().ifBlank { translate("System default") }) {
        change { setFullscreenLyricsFontFamily(it.ifBlank { null }) }
    }
    PreferenceSlider("Font size (px)", lyrics.fullscreenLyricsFontSize?.toDouble() ?: 19.0, 12.0..28.0, 1.0, 19.0, listOf(12.0, 16.0, 20.0, 24.0, 28.0)) {
        change { setFullscreenLyricsFontSize(it.toInt().toUShort()) }
    }
    }
    SettingsGroup(translate("Sidebar")) {
    SettingsText(translate("Font family"), lyrics.lyricsFontFamily.orEmpty(), lyrics.lyricsFontFamily.orEmpty().ifBlank { translate("System default") }) { change { setLyricsFontFamily(it.ifBlank { null }) } }
    PreferenceSlider("Font size (px)", lyrics.lyricsFontSize?.toDouble() ?: 19.0, 12.0..28.0, 1.0, 19.0, listOf(12.0, 16.0, 20.0, 24.0, 28.0)) { change { setLyricsFontSize(it.toInt().toUShort()) } }
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
    val options = player.preferenceOptions ?: return
    val defaults = options.defaultVisualizerAppearance
    fun change(edit: SettingsOwner.() -> Unit) { player.visualizerAppearance(edit) }
    val slot = visualizer.selectedPreset?.toInt() ?: -1
    var colorEdit by remember { mutableStateOf<Int?>(null) }
    var nameAction by remember { mutableStateOf<String?>(null) }
    var presetName by remember { mutableStateOf("") }
    var importing by rememberSaveable { mutableStateOf<Boolean?>(null) }
    var transferText by rememberSaveable { mutableStateOf("") }
    var transferError by remember { mutableStateOf<String?>(null) }
    fun openTransfer(isImport: Boolean) {
        transferText = ""
        transferError = null
        importing = isImport
        if (!isImport) player.exportVisualizer { transferText = it }
    }
    val saveTarget = settings.visualizerSaveTarget
    val userPreset = saveTarget != null
    fun namePreset(action: String) {
        presetName = visualizer.presets.getOrNull(slot)
            ?.let { if (userPreset) it.name else translate(it.name) }.orEmpty()
        nameAction = action
    }
    fun savePreset() {
        if (saveTarget != null) player.visualizerPreset(saveTarget, true) else namePreset("Save")
    }
    val modified = visualizer.presets.getOrNull(slot)?.appearance != appearance
    PresetActions(modifier = Modifier.padding(horizontal = 16.dp), editEnabled = userPreset, saveEnabled = modified,
        save = ::savePreset,
        duplicate = { namePreset("Duplicate") }, rename = { namePreset("Rename") },
        importPreset = { openTransfer(true) }, exportPreset = { openTransfer(false) },
        delete = { saveTarget?.let { target -> change { deleteVisualizerPreset(target) } } },
    )
    SettingsGroup {
        PreferenceChoice(translate("Preset"), slot.toString(), (if (slot < 0) listOf("-1" to presetDisplayName(translate("Custom"), modified)) else emptyList()) + visualizer.presets.mapIndexed { index, preset ->
            val name = if (index < 7) translate(preset.name) else preset.name
            index.toString() to presetDisplayName(name, index == slot && modified)
        },
            select = { if (it != "-1") player.visualizerPreset(it.toULong(), false) })
    }
    SettingsGroup(translate("Spectrum")) {
        PreferenceChoice(translate("FFT size"), settings.playback.visualizerFftSize.toString(),
            listOf(1024, 2048, 4096, 8192, 16384, 32768).map { it.toString() to it.toString() },
            select = { change { setVisualizerFftSize(it.toUInt()) } })
        PreferenceSwitch("Ignore playback volume", settings.playback.visualizerIgnoreVolume) { change { setVisualizerIgnoreVolume(it) } }
        PreferenceChoice(translate("Frequency scale"), appearance.frequencyScale.name,
            VisualizerFrequencyScale.entries.map { it.name to translate(when (it) {
                VisualizerFrequencyScale.ERB -> "ERB"
                VisualizerFrequencyScale.LOGARITHMIC -> "Logarithmic"
                VisualizerFrequencyScale.LINEAR -> "Linear"
            }) }, select = { change { setVisualizerFrequencyScale(VisualizerFrequencyScale.valueOf(it)) } })
        val minFrequencyRange = 20.0..appearance.maxFrequency - 1
        val maxFrequencyRange = appearance.minFrequency + 1..20000.0
        PreferenceSlider("Minimum frequency (Hz)", appearance.minFrequency, minFrequencyRange, 1.0, defaults.minFrequency,
            listOf(minFrequencyRange.start, (minFrequencyRange.start + minFrequencyRange.endInclusive) / 2, minFrequencyRange.endInclusive)) {
            change { setVisualizerMinFrequency(it) }
        }
        PreferenceSlider("Maximum frequency (Hz)", appearance.maxFrequency, maxFrequencyRange, 1.0, defaults.maxFrequency,
            listOf(maxFrequencyRange.start, (maxFrequencyRange.start + maxFrequencyRange.endInclusive) / 2, maxFrequencyRange.endInclusive)) {
            change { setVisualizerMaxFrequency(it) }
        }
        val minLevelRange = -120.0..appearance.maxDecibels - 1
        val maxLevelRange = appearance.minDecibels + 1..0.0
        PreferenceSlider("Minimum level (dB)", appearance.minDecibels, minLevelRange, 1.0, defaults.minDecibels,
            listOf(minLevelRange.start, -60.0, minLevelRange.endInclusive).distinct().filter { it in minLevelRange }) {
            change { setVisualizerMinDecibels(it) }
        }
        PreferenceSlider("Maximum level (dB)", appearance.maxDecibels, maxLevelRange, 1.0, defaults.maxDecibels,
            listOf(maxLevelRange.start, -40.0, maxLevelRange.endInclusive).distinct().filter { it in maxLevelRange }) {
            change { setVisualizerMaxDecibels(it) }
        }
        PreferenceChoice(translate("Weighting"), appearance.weighting.name,
            VisualizerWeighting.entries.map { it.name to when (it) {
                VisualizerWeighting.NONE -> translate("None")
                VisualizerWeighting.A -> "A"
                VisualizerWeighting.C -> "C"
            } }, select = { change { setVisualizerWeighting(VisualizerWeighting.valueOf(it)) } })
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
        VisualizerStyle.RIBBON -> "Ribbon"
        VisualizerStyle.HALO -> "Halo"
    }) },
        select = { change { setVisualizerStyle(VisualizerStyle.valueOf(it)) } })
    PreferenceSlider("Bar count", appearance.barCount.toDouble(), 8.0..256.0, 1.0, defaults.barCount.toDouble(),
        listOf(8.0, 64.0, 128.0, 192.0, 256.0)) { change { setVisualizerBarCount(it.toUInt()) } }
    PreferenceSwitch("Mirrored", appearance.mirrored) { change { setVisualizerMirrored(it) } }
    PreferenceSwitch("Shadow", appearance.shadow) { change { setVisualizerShadow(it) } }
    if (appearance.shadow) {
        PreferenceSlider("Height (%)", appearance.shadowRatio * 100, 0.0..95.0, 1.0, defaults.shadowRatio * 100, listOf(0.0, 50.0, 95.0)) { change { setVisualizerShadowRatio(it / 100.0) } }
        PreferenceSlider("Opacity (%)", appearance.shadowOpacity * 100, 0.0..100.0, 1.0, defaults.shadowOpacity * 100, listOf(0.0, 50.0, 100.0)) { change { setVisualizerShadowOpacity(it / 100.0) } }
        PreferenceSlider("Brightness (%)", appearance.shadowBrightness * 100, 0.0..200.0, 1.0, defaults.shadowBrightness * 100, listOf(0.0, 100.0, 200.0)) { change { setVisualizerShadowBrightness(it / 100.0) } }
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
    PreferenceSlider("Bar spacing (px)", appearance.spacing, 0.0..12.0, .5, defaults.spacing, listOf(0.0, 6.0, 12.0)) { change { setVisualizerSpacing(it) } }
    PreferenceSlider("Opacity (%)", appearance.opacity * 100, 0.0..100.0, 1.0, defaults.opacity * 100, listOf(0.0, 50.0, 100.0)) { change { setVisualizerOpacity(it / 100.0) } }
    PreferenceSlider("Frame rate limit", visualizer.fpsLimit.toDouble(), 1.0..180.0, 1.0, options.defaultVisualizerFpsLimit.toDouble(),
        listOf(1.0, 30.0, 60.0, 120.0, 180.0)) { player.visualizerFrameLimit(it.toInt()) }
    }
    SettingsGroup(translate("Motion")) {
    PreferenceSlider("Rise speed (%)", appearance.rise * 100, 1.0..100.0, 1.0, defaults.rise * 100, listOf(1.0, 50.0, 100.0)) { change { setVisualizerRise(it / 100.0) } }
    PreferenceSlider("Fall speed (%)", appearance.fall * 100, 1.0..100.0, 1.0, defaults.fall * 100, listOf(1.0, 50.0, 100.0)) { change { setVisualizerFall(it / 100.0) } }
    PreferenceSwitch("Show peak markers", appearance.peaks) { change { setVisualizerPeaks(it) } }
    PreferenceSlider("Peak hold (seconds)", appearance.peakHold, 0.0..5.0, .1, defaults.peakHold, listOf(0.0, 2.5, 5.0)) { change { setVisualizerPeakHold(it) } }
    PreferenceSlider("Peak fall speed (% per second)", appearance.peakFall * 100, 1.0..200.0, 1.0, defaults.peakFall * 100, listOf(1.0, 100.0, 200.0)) { change { setVisualizerPeakFall(it / 100.0) } }
    }
    SettingsGroup(translate("Lyrics background opacity")) {
        PreferenceSlider("Fullscreen", appearance.fullscreenLyricsOpacity * 100, 0.0..100.0, 1.0, defaults.fullscreenLyricsOpacity * 100, listOf(0.0, 50.0, 100.0)) { change { setVisualizerFullscreenLyricsOpacity(it / 100.0) } }
        PreferenceSlider("Sidebar", appearance.sidebarLyricsOpacity * 100, 0.0..100.0, 1.0, defaults.sidebarLyricsOpacity * 100, listOf(0.0, 50.0, 100.0)) { change { setVisualizerSidebarLyricsOpacity(it / 100.0) } }
    }
    SettingsGroup {
        SettingsRow(translate("Reset appearance"),
            leading = { RufinIcon("rufin-edit-undo-symbolic", null, Modifier.size(24.dp)) },
            modifier = Modifier.clickable(onClick = player::resetVisualizer))
    }
    nameAction?.let { action ->
        PresetNameDialog(action, presetName, { presetName = it }, { nameAction = null }) {
            change {
                if (action == "Rename") saveTarget?.let { renameVisualizerPreset(it, presetName) }
                else saveVisualizerPreset(presetName)
            }
            nameAction = null
        }
    }
    importing?.let { isImport ->
        val displayName = visualizer.presets.getOrNull(slot)?.let { if (userPreset) it.name else translate(it.name) } ?: translate("Custom")
        PresetTransferDialog(isImport, transferText, { transferText = it; transferError = null },
            transferError, { transferError = it }, "$displayName.json", "application/json", {
                player.model.runAction {
                    try { player.importVisualizer(transferText); importing = null }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { transferError = error.message }
                }
            }, { importing = null })
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
private fun PreferenceSlider(label: String, value: Double, range: ClosedFloatingPointRange<Double>, step: Double, default: Double?,
    marks: List<Double> = emptyList(), change: (Double) -> Unit) {
    val open = range.endInclusive > range.start
    SettingsSliderNumber(translate(label), value, range, step, default = default?.takeIf { it in range },
        marks = if (open) marks else emptyList(), enabled = open, save = change)
}

@Composable
private fun PlayerColorDialog(label: String, initial: Color, onDismiss: () -> Unit, save: (Color) -> Unit) {
    var red by remember { mutableFloatStateOf(initial.red) }
    var green by remember { mutableFloatStateOf(initial.green) }
    var blue by remember { mutableFloatStateOf(initial.blue) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate(label)) }, text = {
        Column {
            Surface(Modifier.fillMaxWidth().height(64.dp), color = Color(red, green, blue)) {}
            PreferenceSlider("Red", red * 255.0, 0.0..255.0, 1.0, null) { red = it.toFloat() / 255 }
            PreferenceSlider("Green", green * 255.0, 0.0..255.0, 1.0, null) { green = it.toFloat() / 255 }
            PreferenceSlider("Blue", blue * 255.0, 0.0..255.0, 1.0, null) { blue = it.toFloat() / 255 }
        }
    }, confirmButton = { TextButton({ save(Color(red, green, blue)) }) { Text(translate("Save")) } },
        dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}
