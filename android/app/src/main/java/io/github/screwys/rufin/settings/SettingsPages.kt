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
import io.github.screwys.rufin.shared.rufin_core.SidebarRouteItem
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerSettingsContent
import io.github.screwys.rufin.player.PlayerSettingsPage
import io.github.screwys.rufin.player.PlaybackPreferences
import io.github.screwys.rufin.sources.SourcesPage
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
    LaunchedEffect(destination) { preferences.refreshApplicationSettings(); player.refreshSettings() }
    val state = preferences.applicationSettings
    if (state == null) { Box(Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) { CircularProgressIndicator() }; return }
    SettingsPage {
        when (destination) {
            MoreDestination.General -> {
                SettingsGroup {
                    PreferenceChoice(translate("Language"), preferences.state?.language ?: "system",
                        preferences.state?.languages.orEmpty().map { it.id to if (it.id == "system") translate("System default") else it.title }, select = preferences::selectLanguage)
                }
                SettingsGroup(translate("App settings")) {
                    PreferenceChoice(translate("Starting page"), state.startingPage.name,
                        browse.routes.map { it.item.name to it.title },
                        select = { page -> preferences.changePreference { setStartingPage(SidebarRouteItem.valueOf(page)) } })
                    PreferenceChoice(translate("Update check interval"), state.releaseCheckIntervalHours.toString(),
                        listOf("1" to translate("1 hour"), "6" to translate("6 hours"), "12" to translate("12 hours"), "24" to translate("24 hours")),
                        select = { preferences.changePreference { setReleaseCheckIntervalHours(it.toUInt()) } })
                    SettingsLink(null, translate("Listening overview")) { navigate(MoreDestination.OverviewSettings) }
                }
                SettingsGroup(translate("Notifications")) {
                    SettingsSwitch(translate("Control notifications"), state.controlNotificationsEnabled) { preferences.changePreference { setControlNotificationsEnabled(it) } }
                    SettingsSwitch(translate("Release notifications"), state.releaseNotificationsEnabled) { preferences.changePreference { setReleaseNotificationsEnabled(it) } }
                }
                SettingsGroup(translate("Metadata")) {
                    SettingsSwitch(translate("Prefer distinct track covers"), state.preferDistinctTrackCovers) { preferences.changePreference { setPreferDistinctTrackCovers(it) } }
                    SettingsSwitch(translate("External metadata lookup"), state.externalMetadataEnabled) { preferences.changePreference { setExternalMetadataEnabled(it) } }
                    player.settings?.lyrics?.let { lyrics -> SettingsSwitch(translate("External lyrics lookup"), lyrics.externalLyricsEnabled) { player.lyricsPreference { setExternalLyricsEnabled(it) } } }
                }
                val links = state.externalSiteLinks
                SettingsGroup(translate("External site links")) {
                    SettingsSwitch(translate("Show external site links"), links.enabled) { preferences.changePreference { setExternalSiteLinks(links.copy(enabled = it)) } }
                    SettingsSwitch("Last.fm", links.lastfm) { preferences.changePreference { setExternalSiteLinks(links.copy(lastfm = it)) } }
                    SettingsSwitch("MusicBrainz", links.musicbrainz) { preferences.changePreference { setExternalSiteLinks(links.copy(musicbrainz = it)) } }
                    SettingsSwitch(translate("Server"), links.server) { preferences.changePreference { setExternalSiteLinks(links.copy(server = it)) } }
                }
                BackupsPage(model, preferences, embedded = true, openSettings = { navigate(MoreDestination.BackupSettings) })
            }
            MoreDestination.Playback -> {
                PlaybackPreferences(player, preferences, output)
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
                    SettingsSwitch(translate("Private mode"), state.privateMode) { preferences.changePreference { setPrivateMode(it) } }
                    SettingsSwitch(translate("Proxy casting through Rufin"), state.castProxyEnabled) { preferences.changePreference { setCastProxyEnabled(it) } }
                    PreferenceChoice(translate("Secret storage"), if (state.secretStorageMode == io.github.screwys.rufin.shared.secrets.SecretStorageMode.SYSTEM_KEYRING) "system-keyring" else "config-file", listOf("system-keyring" to translate("System keyring"), "config-file" to translate("Legacy")), select = preferences::changeSecretStorage)
                }
            }
            else -> Unit
        }
    }

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
