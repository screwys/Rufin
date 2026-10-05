package io.github.screwys.rufin.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerSettingsContent
import io.github.screwys.rufin.player.PlayerSettingsPage
import io.github.screwys.rufin.sources.SourcesPage
import org.json.JSONObject
import kotlin.math.round

@Composable
internal fun SettingsIndexPage(navigate: (MoreDestination) -> Unit) {
    SettingsPage {
        SettingsGroup {
            SettingsLink("rufin-preferences-system-symbolic", translate("General")) { navigate(MoreDestination.General) }
            SettingsLink("rufin-preferences-desktop-appearance-symbolic", translate("Appearance")) { navigate(MoreDestination.Appearance) }
            SettingsLink("rufin-music-queue-symbolic", translate("Playback")) { navigate(MoreDestination.Playback) }
            SettingsLink("rufin-download-symbolic", translate("Downloads")) { navigate(MoreDestination.DownloadSettings) }
            SettingsLink("rufin-system-lock-screen-symbolic", translate("Privacy and Security")) { navigate(MoreDestination.Privacy) }
        }
    }
}

@Composable
internal fun SettingsLink(icon: String?, title: String, summary: String? = null, open: () -> Unit) {
    SettingsRow(title, summary = summary,
        leading = icon?.let { { RufinIcon(it, null, Modifier.size(24.dp)) } },
        trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) }, modifier = Modifier.clickable(onClick = open))
}

@Composable
internal fun SettingsSwitch(title: String, checked: Boolean, summary: String? = null, change: (Boolean) -> Unit) {
    SettingsRow(title, summary = summary,
        trailing = { Switch(checked, null) },
        modifier = Modifier.toggleable(checked, role = Role.Switch, onValueChange = change))
}

@Composable
internal fun SettingsNumber(title: String, value: Double, range: ClosedFloatingPointRange<Double>, integer: Boolean = true, enabled: Boolean = true, save: (Any) -> Unit) {
    var edit by remember { mutableStateOf(false) }
    var text by remember(value) { mutableStateOf(if (integer) value.toLong().toString() else value.toString()) }
    SettingsRow(title, summary = if (integer) value.toLong().toString() else value.toString(),
        modifier = Modifier.clickable(enabled = enabled) { edit = true })
    if (edit) AlertDialog(onDismissRequest = { edit = false }, title = { Text(title) }, text = {
        OutlinedTextField(text, { text = it }, singleLine = true, colors = settingsTextFieldColors(),
            keyboardOptions = KeyboardOptions(keyboardType = if (integer) KeyboardType.Number else KeyboardType.Decimal))
    }, confirmButton = { TextButton({ text.toDoubleOrNull()?.let { save(if (integer) it.toLong() else it) }; edit = false },
        enabled = text.toDoubleOrNull()?.let { it in range && (!integer || it % 1.0 == 0.0) } == true) { Text(translate("Save")) } },
        dismissButton = { TextButton({ edit = false }) { Text(translate("Cancel")) } })
}

@Composable
internal fun SettingsSliderNumber(title: String, value: Double, range: ClosedFloatingPointRange<Double>,
    step: Double, integer: Boolean = false, unit: String? = null, save: (Any) -> Unit) {
    var draft by remember(value) { mutableDoubleStateOf(value) }
    var text by remember(value) { mutableStateOf(numberText(value)) }
    var editing by remember { mutableStateOf(false) }
    val focus = LocalFocusManager.current
    val parsed = text.replace(',', '.').toDoubleOrNull()
    val valid = parsed != null && parsed in range && (!integer || parsed % 1.0 == 0.0)
    fun commit(next: Double) {
        draft = next
        text = numberText(next)
        save(if (integer) next.toInt() else next)
    }
    Surface(Modifier.fillMaxWidth(), color = MaterialTheme.colorScheme.surfaceContainer, shape = MaterialTheme.shapes.small) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
            OutlinedTextField(text, { text = it; editing = true }, Modifier.width(108.dp)
                .onFocusChanged { state ->
                    if (!state.isFocused && editing) {
                        if (valid) commit(parsed!!)
                        editing = false
                    }
                }, singleLine = true, isError = editing && !valid, colors = settingsTextFieldColors(),
                suffix = unit?.let { { Text(it, style = MaterialTheme.typography.labelMedium) } },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { if (valid) { commit(parsed!!); editing = false; focus.clearFocus() } }))
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton({ commit(java.math.BigDecimal.valueOf(draft).subtract(java.math.BigDecimal.valueOf(step)).toDouble().coerceIn(range)) }, enabled = draft > range.start) {
                RufinIcon("rufin-list-remove-symbolic", translate("Decrease"), Modifier.size(20.dp))
            }
            Slider(draft.toFloat(), {
                draft = java.math.BigDecimal.valueOf(step).multiply(java.math.BigDecimal.valueOf(round(it.toDouble() / step)))
                    .toDouble().coerceIn(range)
                text = numberText(draft)
            }, Modifier.weight(1f), valueRange = range.start.toFloat()..range.endInclusive.toFloat(),
                colors = SliderDefaults.colors(inactiveTrackColor = MaterialTheme.colorScheme.onSurface.copy(alpha = .24f)),
                onValueChangeFinished = { commit(draft) })
            IconButton({ commit(java.math.BigDecimal.valueOf(draft).add(java.math.BigDecimal.valueOf(step)).toDouble().coerceIn(range)) }, enabled = draft < range.endInclusive) {
                RufinIcon("rufin-list-add-symbolic", translate("Increase"), Modifier.size(20.dp))
            }
        }
    }
    }
}

private fun numberText(value: Double): String = java.math.BigDecimal.valueOf(value).stripTrailingZeros().toPlainString()

@Composable
internal fun AppSettingsPage(destination: MoreDestination, model: RufinConnection, preferences: PreferencesConnection,
    player: PlayerConnection, browse: BrowseConnection, output: () -> Unit, addSource: () -> Unit, editSource: (String) -> Unit,
    navigate: (MoreDestination) -> Unit) {
    LaunchedEffect(destination) { preferences.refreshApplicationSettings() }
    val state = preferences.applicationSettings
    if (state == null) { Box(Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) { CircularProgressIndicator() }; return }
    val playback = state.optJSONObject("playback") ?: JSONObject()
    var confirmEbu by remember { mutableStateOf(false) }
    fun bool(key: String, value: Boolean) = preferences.changePreference(key, value)
    SettingsPage {
        when (destination) {
            MoreDestination.General -> {
                SettingsGroup {
                    PreferenceChoice(translate("Language"), preferences.state?.language ?: "system",
                        preferences.state?.languages.orEmpty().map { it.id to if (it.id == "system") translate("System default") else it.title }, select = preferences::selectLanguage)
                }
                SettingsGroup(translate("App settings")) {
                    PreferenceChoice(translate("Update check interval"), state.optInt("release_check_interval_hours", 6).toString(),
                        listOf("1" to translate("1 hour"), "6" to translate("6 hours"), "12" to translate("12 hours"), "24" to translate("24 hours")),
                        select = { preferences.changePreference("release_check_interval_hours", it.toInt()) })
                    SettingsLink(null, translate("Listening overview")) { navigate(MoreDestination.OverviewSettings) }
                }
                SettingsGroup(translate("Notifications")) {
                    SettingsSwitch(translate("Control notifications"), state.optBoolean("control_notifications_enabled")) { bool("control_notifications_enabled", it) }
                    SettingsSwitch(translate("Release notifications"), state.optBoolean("release_notifications_enabled")) { bool("release_notifications_enabled", it) }
                }
                SettingsGroup(translate("Metadata")) {
                    SettingsSwitch(translate("Prefer distinct track covers"), state.optBoolean("prefer_distinct_track_covers")) { bool("prefer_distinct_track_covers", it) }
                    SettingsSwitch(translate("External metadata lookup"), state.optBoolean("external_metadata_enabled")) { bool("external_metadata_enabled", it) }
                    SettingsSwitch(translate("External lyrics lookup"), state.optBoolean("external_lyrics_enabled")) { bool("lyrics.external_lyrics_enabled", it) }
                }
                val links = state.optJSONObject("external_site_links") ?: JSONObject()
                SettingsGroup(translate("External site links")) {
                    SettingsSwitch(translate("Show external site links"), links.optBoolean("enabled")) { bool("external_site_links.enabled", it) }
                    SettingsSwitch("Last.fm", links.optBoolean("lastfm")) { bool("external_site_links.lastfm", it) }
                    SettingsSwitch("MusicBrainz", links.optBoolean("musicbrainz")) { bool("external_site_links.musicbrainz", it) }
                    SettingsSwitch(translate("Server"), links.optBoolean("server")) { bool("external_site_links.server", it) }
                }
                BackupsPage(model, preferences, embedded = true, openSettings = { navigate(MoreDestination.BackupSettings) })
            }
            MoreDestination.Playback -> {
                SettingsGroup(translate("Queue and transitions")) {
                    SettingsSelection(translate("Transition mode"), playback.optString("transition_mode", "Gapless"),
                        listOf("Gapless" to translate("Gapless"), "Crossfade" to translate("Crossfade")), select = { preferences.changePreference("playback.transition_mode", it) })
                    if (playback.optString("transition_mode") == "Crossfade") SettingsSliderNumber(translate("Crossfade duration"), playback.optDouble("crossfade_seconds", 5.0), 1.0..30.0, 1.0, integer = true, unit = "s") { preferences.changePreference("playback.crossfade_seconds", it) }
                    SettingsSwitch(translate("Skip same-album crossfade"), playback.optBoolean("skip_same_album_crossfade")) { bool("playback.skip_same_album_crossfade", it) }
                    SettingsSwitch(translate("Audio fade on play/pause"), playback.optBoolean("audio_fade_on_status_change")) { bool("playback.audio_fade_on_status_change", it) }
                    SettingsSwitch(translate("Clearing queue also clears the current song"), state.optBoolean("clear_queue_includes_current")) { bool("clear_queue_includes_current", it) }
                    SettingsNumber(translate("Auto DJ refill threshold"), state.optDouble("auto_dj_refill_threshold", 1.0), 1.0..10.0) { preferences.changePreference("auto_dj_refill_threshold", it) }
                }
                SettingsGroup(translate("Audio")) {
                    SettingsRow(translate("Audio output"), modifier = Modifier.clickable(onClick = output))
                    PreferenceChoice(translate("Loudness normalization"), playback.optString("loudness_normalization", "ReplayGain"), listOf("Off" to translate("Off"), "ReplayGain" to "ReplayGain", "EbuR128" to "EBU R128"), select = {
                        if (it == "EbuR128" && playback.optString("loudness_normalization") != "EbuR128") confirmEbu = true
                        else preferences.changePreference("playback.loudness_normalization", it)
                    })
                    PreferenceChoice(translate("Normalization scope"), playback.optString("loudness_normalization_scope", "Album"), listOf("Track" to translate("Track"), "Album" to translate("Album")), select = { preferences.changePreference("playback.loudness_normalization_scope", it) })
                    SettingsNumber(translate("Target loudness"), playback.optDouble("ebu_r128_target_lufs", -23.0), -48.0..0.0, false) { preferences.changePreference("playback.ebu_r128_target_lufs", it) }
                    SettingsSwitch(translate("Write EBU R128 tags to files"), playback.optBoolean("write_ebu_r128_tags")) { bool("playback.write_ebu_r128_tags", it) }
                    PreferenceChoice(translate("Volume scale"), playback.optString("volume_scale", "Perceptual"), listOf("Perceptual" to translate("Perceptual"), "Linear" to translate("Linear")), select = { preferences.changePreference("playback.volume_scale", it) })
                    val bitrate = playback.optJSONObject("stream_quality")?.optInt("MaxBitrateKbps")?.toString() ?: "original"
                    PreferenceChoice(translate("Stream quality"), bitrate,
                        listOf("original" to translate("Original")) + listOf(320, 256, 192, 128).map { it.toString() to "$it kbps" },
                        select = { preferences.changePreference("playback.stream_quality", if (it == "original") "Original" else JSONObject().put("MaxBitrateKbps", it.toInt())) })
                    SettingsSliderNumber(translate("Playback speed"), playback.optDouble("playback_rate", 1.0), 0.5..2.0, .05, unit = "×") { preferences.changePreference("playback.playback_rate", it) }
                }
                SettingsGroup {
                    SettingsSwitch(translate("Dynamic background"), state.optBoolean("fullscreen_dynamic_background")) { bool("fullscreen_dynamic_background", it) }
                    SettingsSwitch(translate("Enable background image"), state.optBoolean("fullscreen_background_image")) { bool("fullscreen_background_image", it) }
                    SettingsSwitch(translate("Waveform seekbar"), state.optBoolean("seekbar_waveform_enabled")) { bool("seekbar_waveform_enabled", it) }
                }
                Text(translate("Equalizer"), style = MaterialTheme.typography.titleMedium)
                PlayerSettingsContent(player, PlayerSettingsPage.EQUALIZER, embedded = true)
                Text(translate("Lyrics"), style = MaterialTheme.typography.titleMedium)
                PlayerSettingsContent(player, PlayerSettingsPage.LYRICS, embedded = true)
                Text(translate("Visualizer"), style = MaterialTheme.typography.titleMedium)
                PlayerSettingsContent(player, PlayerSettingsPage.VISUALIZER, embedded = true)
            }
            MoreDestination.Integrations -> {
                ScrobblingPage(preferences, embedded = true)
                Text(translate("File connections"), style = MaterialTheme.typography.titleMedium)
                SourcesPage(model, addSource, editSource, embedded = true, connectionsOnly = true)
            }
            MoreDestination.Privacy -> {
                SettingsGroup {
                    SettingsSwitch(translate("Private mode"), state.optBoolean("private_mode")) { bool("private_mode", it) }
                    SettingsSwitch(translate("Proxy casting through Rufin"), state.optBoolean("cast_proxy_enabled")) { bool("cast_proxy_enabled", it) }
                    PreferenceChoice(translate("Secret storage"), state.optString("secret_storage_mode"), listOf("system-keyring" to translate("System keyring"), "config-file" to translate("Legacy")), select = preferences::changeSecretStorage)
                }
            }
            else -> Unit
        }
    }
    if (confirmEbu) AlertDialog(onDismissRequest = { confirmEbu = false },
        title = { Text(translate("Enable EBU R128 Analysis?")) },
        text = { Text(translate("Rufin will calculate the missing EBU R128 metadata for the whole library. This can use significant CPU, battery, and network bandwidth for a long time.")) },
        confirmButton = { TextButton({ confirmEbu = false; preferences.changePreference("playback.loudness_normalization", "EbuR128") }) { Text(translate("Enable")) } },
        dismissButton = { TextButton({ confirmEbu = false }) { Text(translate("Cancel")) } })
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun SettingsSelection(title: String, selected: String, options: List<Pair<String, String>>, select: (String) -> Unit) {
    Surface(Modifier.fillMaxWidth(), color = MaterialTheme.colorScheme.surfaceContainer, shape = MaterialTheme.shapes.small) {
    Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(title, style = MaterialTheme.typography.bodyLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            options.forEach { (id, name) -> FilterChip(selected == id, { select(id) }, label = { Text(name) }, colors = settingsChipColors()) }
        }
    }
    }
}
