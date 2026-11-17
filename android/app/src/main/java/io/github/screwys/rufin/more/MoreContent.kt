package io.github.screwys.rufin.more

import io.github.screwys.rufin.settings.CustomizationSettingsPage
import io.github.screwys.rufin.player.PlaybackPreferences
import io.github.screwys.rufin.settings.ThemeSettingsPage
import io.github.screwys.rufin.settings.StorageSettingsPage
import io.github.screwys.rufin.settings.OverviewSettingsPage
import io.github.screwys.rufin.settings.BackupSettingsPage
import io.github.screwys.rufin.activity.ListeningOverviewPage
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerSettingsContent
import io.github.screwys.rufin.player.PlayerSettingsPage
import androidx.compose.animation.AnimatedContent
import io.github.screwys.rufin.ui.pageMotion
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.controller.ControllerPage
import io.github.screwys.rufin.connect.ConnectPage
import io.github.screwys.rufin.sources.SourcesPage
import io.github.screwys.rufin.settings.AppSettingsPage
import io.github.screwys.rufin.settings.IntegrationDetailPage
import io.github.screwys.rufin.settings.IntegrationPage
import io.github.screwys.rufin.settings.IntegrationsPage
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.shared.playback.PlaybackTransitionMode
import io.github.screwys.rufin.shared.playback.LoudnessNormalization
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.Alignment
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.downloads.DownloadsPage
import io.github.screwys.rufin.downloads.DownloadSettingsPage
import io.github.screwys.rufin.diagnostics.TroubleshootingPage
import io.github.screwys.rufin.releases.VersionHistoryPage

import androidx.compose.foundation.layout.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.core.translate

internal enum class MoreDestination(val title: String) {
    Root("Settings"), Overview("Listening overview"), OverviewSettings("Listening overview"),
    Versions("Version History"), Troubleshooting("Troubleshooting"),
    General("General"), Customization("Customization"), Theme("Theme"), Integrations("Integrations"), Integration("Integrations"),
    Playback("Playback"), Equalizer("Equalizer"), Library("Library"), Downloads("Downloads"),
    Connect("Connect"), Controller("Controller"), Storage("Storage"), Privacy("Privacy & Security"),
    DownloadSettings("Downloads"),
    BackupSettings("Backup Settings");

    // The sidebar's rail opens Settings and these three pages directly.
    val parent get() = when (this) {
        Root, Overview, Versions, Troubleshooting -> null
        Integration -> Integrations
        BackupSettings -> Privacy
        OverviewSettings -> General
        DownloadSettings -> Downloads
        else -> Root
    }
    val depth: Int get() = parent?.let { it.depth + 1 } ?: 0
}

@Composable
internal fun MoreContent(model: RufinConnection, preferences: PreferencesConnection, player: PlayerConnection, browse: BrowseConnection,
    destination: MoreDestination, navigate: (MoreDestination) -> Unit,
    /** The integration page Integration shows, and how one opens. */
    integration: IntegrationPage?, openIntegration: (IntegrationPage) -> Unit, back: () -> Unit,
    onAddSource: () -> Unit, onEditSource: (String) -> Unit, onOpen: () -> Unit, onSaveLog: () -> Unit, onOutput: () -> Unit,
    navigationDepth: Int,
) {
    val more = model.more
    val reduceMotion = LocalReduceMotion.current
    val destinations = rememberSaveableStateHolder()
    LaunchedEffect(destination, more.bridge) {
        more.overviewVisible = destination == MoreDestination.Overview
        if (more.overviewVisible && more.bridge != null) more.prepareOverview()
    }
    DisposableEffect(more) { onDispose { more.overviewVisible = false } }
    AnimatedContent(destination to navigationDepth, Modifier.fillMaxSize(), transitionSpec = {
        pageMotion(targetState.second > initialState.second, reduceMotion)
    }, contentKey = { it.first }, label = "moreNavigation") { location ->
    val visible = location.first
    destinations.SaveableStateProvider(visible.name) {
    when (visible) {
        MoreDestination.Root -> SettingsPage {
            // Each page's row says what is set on it, where one line can.
            val language = preferences.state?.let { state ->
                state.languages.firstOrNull { it.id == state.language && it.id != "system" }?.title ?: translate("System default")
            }
            val start = preferences.applicationSettings?.let { settings -> browse.routes.firstOrNull { it.item.name == settings.startingPage.name }?.title }
            val playback = player.settings?.playback?.let { playback ->
                listOf(translate(if (playback.transitionMode == PlaybackTransitionMode.CROSSFADE) "Crossfade" else "Gapless"),
                    when (playback.loudnessNormalization) {
                        LoudnessNormalization.OFF -> null
                        LoudnessNormalization.REPLAY_GAIN -> "ReplayGain"
                        LoudnessNormalization.EBU_R128 -> "EBU R128"
                    })
            }
            SettingsGroup {
                SettingsCategory("rufin-preferences-system-symbolic", "General", listOfNotNull(language, start)) { navigate(MoreDestination.General) }
                SettingsCategory("rufin-sliders-2-symbolic", "Customization") { navigate(MoreDestination.Customization) }
                SettingsCategory("rufin-preferences-desktop-appearance-symbolic", "Theme") { navigate(MoreDestination.Theme) }
            }
            SettingsGroup {
                SettingsCategory("rufin-network-workgroup-symbolic", "Integrations") { navigate(MoreDestination.Integrations) }
            }
            SettingsGroup {
                SettingsCategory("rufin-music-queue-symbolic", "Playback", playback.orEmpty().filterNotNull()) { navigate(MoreDestination.Playback) }
                SettingsCategory("rufin-sliders-vertical-symbolic", "Equalizer") { navigate(MoreDestination.Equalizer) }
            }
            SettingsGroup {
                SettingsCategory("rufin-library-symbolic", "Library", model.sources?.sources.orEmpty().map { it.name }.filter { it.isNotBlank() }, ", ") { navigate(MoreDestination.Library) }
                SettingsCategory("rufin-download-symbolic", "Downloads") { navigate(MoreDestination.Downloads) }
            }
            SettingsGroup {
                SettingsCategory("rufin-network-hotspot-symbolic", "Connect") { navigate(MoreDestination.Connect) }
                SettingsCategory("rufin-phonelink-symbolic", "Controller") { navigate(MoreDestination.Controller) }
            }
            SettingsGroup {
                SettingsCategory("rufin-drive-harddisk-symbolic", "Storage") { navigate(MoreDestination.Storage) }
                SettingsCategory("rufin-system-lock-screen-symbolic", "Privacy & Security") { navigate(MoreDestination.Privacy) }
            }
        }
        MoreDestination.Library -> SourcesPage(model, onAddSource, onEditSource)
        MoreDestination.General,
        MoreDestination.Privacy -> AppSettingsPage(visible, model, preferences, player, browse, navigate)
        MoreDestination.Playback -> PlaybackPreferences(player, preferences, model.sources?.sources.orEmpty(), onOutput)
        MoreDestination.Integrations -> IntegrationsPage(model, preferences, player, openIntegration)
        MoreDestination.Integration -> integration?.let { page -> key(page.id) {
            IntegrationDetailPage(model, preferences, player, page, openIntegration, back)
        } } ?: LaunchedEffect(Unit) { back() }
        MoreDestination.Downloads -> DownloadsPage(model, player) { navigate(MoreDestination.DownloadSettings) }
        MoreDestination.DownloadSettings -> DownloadSettingsPage(model, preferences)
        MoreDestination.BackupSettings -> BackupSettingsPage(model, preferences)
        MoreDestination.OverviewSettings -> OverviewSettingsPage(preferences)
        MoreDestination.Customization -> CustomizationSettingsPage(preferences, browse, player)
        MoreDestination.Theme -> ThemeSettingsPage(preferences)
        MoreDestination.Storage -> StorageSettingsPage(model, preferences)
        MoreDestination.Equalizer -> PlayerSettingsContent(player, PlayerSettingsPage.EQUALIZER)
        MoreDestination.Controller -> ControllerPage(model)
        MoreDestination.Connect -> ConnectPage(model)
        MoreDestination.Overview -> ListeningOverviewPage(model, more, player)
        MoreDestination.Versions -> VersionHistoryPage(more)
        MoreDestination.Troubleshooting -> TroubleshootingPage(model, onSaveLog)
    }
    }
    }
}

/** A page of Settings on its root: its icon on a tonal tile, its name, and what is set there. */
@Composable
private fun SettingsCategory(icon: String, title: String, facts: List<String> = emptyList(), separator: String = " · ", open: () -> Unit) {
    SettingsRow(translate(title), summary = facts.joinToString(separator).takeIf { it.isNotEmpty() }, leading = {
        Box(Modifier.size(36.dp).background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(12.dp)),
            contentAlignment = Alignment.Center) {
            RufinIcon(icon, null, Modifier.size(20.dp), MaterialTheme.colorScheme.primary)
        }
    }, modifier = Modifier.clickable(onClick = open))
}
