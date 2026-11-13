package io.github.screwys.rufin.settings

import io.github.screwys.rufin.ui.PageLoading

import io.github.screwys.rufin.ui.RufinSlider

import io.github.screwys.rufin.ui.ChoiceChip


import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.clickable
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.R
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.platform.AppIcon
import io.github.screwys.rufin.shared.rufin_core.SecretStorageMode
import io.github.screwys.rufin.shared.rufin_core.SidebarRouteItem
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.AndroidCastNetwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlaybackPreferences
import kotlin.math.round
import kotlin.math.roundToInt

@Composable
internal fun SettingsLink(icon: String?, title: String, summary: String? = null, open: () -> Unit) {
    SettingsRow(title, summary = summary,
        leading = icon?.let { { RufinIcon(it, null, Modifier.size(24.dp)) } },
        trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) }, modifier = Modifier.clickable(onClick = open))
}

@Composable
internal fun SettingsSwitch(title: String, checked: Boolean, summary: String? = null, enabled: Boolean = true,
    change: (Boolean) -> Unit) {
    SettingsRow(title, summary = summary,
        trailing = { Switch(checked, null, enabled = enabled) },
        modifier = Modifier.alpha(if (enabled) 1f else .38f).toggleable(checked, enabled = enabled, role = Role.Switch, onValueChange = change))
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
    step: Double, integer: Boolean = false, unit: String? = null, enabled: Boolean = true, continuous: Boolean = false,
    marks: (@Composable () -> Unit)? = null, save: (Any) -> Unit) {
    var draft by remember(value) { mutableDoubleStateOf(value) }
    val numberFormat = remember(step) {
        java.text.NumberFormat.getNumberInstance().apply {
            maximumFractionDigits = java.math.BigDecimal.valueOf(step).stripTrailingZeros().scale().coerceAtLeast(0)
            isGroupingUsed = false
        }
    }
    val displayValue = numberFormat.format(draft) + unit?.let { " $it" }.orEmpty()
    val colors = MaterialTheme.colorScheme
    Surface(Modifier.fillMaxWidth(), color = colors.surfaceContainer, shape = RoundedCornerShape(5.dp)) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                Text(title, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge,
                    color = colors.onSurface.copy(alpha = if (enabled) 1f else .38f))
                Surface(color = if (enabled) colors.secondaryContainer else Color.Transparent,
                    shape = MaterialTheme.shapes.small) {
                    Text(displayValue, Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                        style = MaterialTheme.typography.labelLarge.copy(fontFeatureSettings = "tnum"),
                        color = if (enabled) colors.onSecondaryContainer else colors.onSurface.copy(alpha = .38f))
                }
            }
            RufinSlider(draft.toFloat(), {
                draft = java.math.BigDecimal.valueOf(step).multiply(java.math.BigDecimal.valueOf(round(it.toDouble() / step)))
                    .toDouble().coerceIn(range)
            }, Modifier.fillMaxWidth().semantics { contentDescription = title; stateDescription = displayValue },
                enabled = enabled, valueRange = range.start.toFloat()..range.endInclusive.toFloat(),
                steps = if (continuous) 0 else ((range.endInclusive - range.start) / step).roundToInt().minus(1).coerceAtLeast(0),
                onValueChangeFinished = { save(if (integer) draft.toInt() else draft) })
            marks?.invoke()
        }
    }
}

@Composable
internal fun AppSettingsPage(destination: MoreDestination, model: RufinConnection, preferences: PreferencesConnection,
    player: PlayerConnection, browse: BrowseConnection, output: () -> Unit, navigate: (MoreDestination) -> Unit) {
    var castNetworks by remember { mutableStateOf<List<AndroidCastNetwork>>(emptyList()) }
    LaunchedEffect(destination, model.ready, player.bridge) {
        if (destination == MoreDestination.Privacy && model.ready) castNetworks = player.castingNetworks()
    }
    val state = preferences.applicationSettings
    if (state == null) { PageLoading(); return }
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
                    AppIconChoice()
                    PreferenceChoice(translate("Update check interval"), state.releaseCheckIntervalHours.toString(),
                        listOf("1" to translate("1 hour"), "6" to translate("6 hours"), "12" to translate("12 hours"), "24" to translate("24 hours")),
                        select = { preferences.changePreference { setReleaseCheckIntervalHours(it.toUInt()) } })
                    SettingsSwitch(translate("Allow permanently deleting tracks"), state.allowPermanentDelete) { preferences.changePreference { setAllowPermanentDelete(it) } }
                    SettingsLink(null, translate("Listening overview")) { navigate(MoreDestination.OverviewSettings) }
                }
                SettingsGroup(translate("Notifications")) {
                    SettingsSwitch(translate("Control notifications"), state.controlNotificationsEnabled) { preferences.changePreference { setControlNotificationsEnabled(it) } }
                    SettingsSwitch(translate("Release notifications"), state.releaseNotificationsEnabled) { preferences.changePreference { setReleaseNotificationsEnabled(it) } }
                }
                SettingsGroup(translate("Metadata")) {
                    SettingsSwitch(translate("Prefer distinct track covers"), state.preferDistinctTrackCovers) { preferences.changePreference { setPreferDistinctTrackCovers(it) } }
                    SettingsSwitch(translate("Use sort tags"), state.useSortTags) { preferences.changePreference { setUseSortTags(it) } }
                }
            }
            MoreDestination.Playback -> PlaybackPreferences(player, preferences, output)
            MoreDestination.Privacy -> {
                SettingsGroup {
                    SettingsSwitch(translate("Private mode"), state.privateMode) { preferences.changePreference { setPrivateMode(it) } }
                    SettingsSwitch(translate("Proxy casting through Rufin"), state.castProxyEnabled) { preferences.changePreference { setCastProxyEnabled(it) } }
                    PreferenceChoice(translate("Casting network"), preferences.castNetworkInterface.orEmpty(),
                        listOf("" to translate("Automatic")) + castNetworks.map { it.id to "${it.name} (${it.address})" },
                        select = { network -> preferences.changePreference { setCastNetworkInterface(network.takeIf { it.isNotEmpty() }) } })
                    PreferenceChoice(translate("Secret storage"), state.secretStorageMode, listOf(SecretStorageMode.SYSTEM_KEYRING to translate("System keyring"), SecretStorageMode.CONFIG_FILE to translate("Legacy")), select = preferences::changeSecretStorage)
                }
                BackupsPage(model, preferences, embedded = true) { navigate(MoreDestination.BackupSettings) }
            }
            else -> Unit
        }
    }

}

/** The launcher icon; the symbolic tiles suit phone themes that leave the colored icon as is. */
@Composable
private fun AppIconChoice() {
    val context = LocalContext.current
    val icon = remember { AppIcon.current(context) }
    PreferenceChoice(translate("App icon"), icon,
        listOf(AppIcon.Colored to translate("Colored"), AppIcon.Dark to translate("Dark"), AppIcon.Light to translate("Light")),
        preview = { choice ->
            if (choice == AppIcon.Colored) Image(painterResource(R.drawable.app_icon), null, Modifier.size(24.dp))
            else Box(Modifier.size(24.dp).clip(RoundedCornerShape(6.dp)).background(if (choice == AppIcon.Dark) Color(0xFF252525) else Color.White),
                contentAlignment = Alignment.Center) {
                // The layer pads the glyph for launcher masks; the tile shows it at launcher size.
                Image(painterResource(if (choice == AppIcon.Dark) R.drawable.launcher_glyph_light else R.drawable.launcher_glyph_dark), null, Modifier.requiredSize(40.dp))
            }
        },
        rowPreview = false, select = { if (it != icon) AppIcon.select(context, it) })
}
