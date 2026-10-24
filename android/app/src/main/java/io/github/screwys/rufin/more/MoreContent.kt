package io.github.screwys.rufin.more

import io.github.screwys.rufin.settings.CustomizationSettingsPage
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
import io.github.screwys.rufin.settings.RouteSettingsPage
import io.github.screwys.rufin.controller.ControllerPage
import io.github.screwys.rufin.connect.ConnectPage
import io.github.screwys.rufin.sources.SourcesPage
import io.github.screwys.rufin.settings.AppSettingsPage
import io.github.screwys.rufin.settings.HomeSettingsPage
import io.github.screwys.rufin.settings.SettingsPage
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsLink
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
    Root("More"), Overview("Listening overview"), OverviewSettings("Listening overview"),
    Versions("Version History"), Troubleshooting("Troubleshooting"),
    General("General"), Customization("Customization"), Theme("Theme"), Integrations("Integrations"),
    Playback("Playback"), Equalizer("Equalizer"), Library("Library"), Downloads("Downloads"),
    Connect("Connect"), Controller("Controller"), Storage("Storage"), Privacy("Privacy & Security"),
    Routes("Library"), Lyrics("Lyrics"), Visualizer("Visualizer"), DownloadSettings("Downloads"),
    BackupSettings("Backup Settings"), HomeSettings("Home Blocks");

    val parent get() = when (this) {
        Root -> null
        Routes, HomeSettings -> Customization
        Lyrics, Visualizer -> Playback
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
            SettingsGroup {
                SettingsLink("rufin-activity-overview-symbolic", translate("Listening overview")) { navigate(MoreDestination.Overview) }
                SettingsLink("rufin-appointment-new-symbolic", translate("Version History")) { navigate(MoreDestination.Versions) }
                SettingsLink("rufin-utilities-terminal-symbolic", translate("Troubleshooting")) { navigate(MoreDestination.Troubleshooting) }
            }
            SettingsGroup(translate("Settings")) {
                SettingsLink("rufin-preferences-system-symbolic", translate("General")) { navigate(MoreDestination.General) }
                SettingsLink("rufin-sliders-2-symbolic", translate("Customization")) { navigate(MoreDestination.Customization) }
                SettingsLink("rufin-preferences-desktop-appearance-symbolic", translate("Theme")) { navigate(MoreDestination.Theme) }
                SettingsLink("rufin-network-workgroup-symbolic", translate("Integrations")) { navigate(MoreDestination.Integrations) }
                SettingsLink("rufin-music-queue-symbolic", translate("Playback")) { navigate(MoreDestination.Playback) }
                SettingsLink("rufin-sliders-vertical-symbolic", translate("Equalizer")) { navigate(MoreDestination.Equalizer) }
                SettingsLink("rufin-library-symbolic", translate("Library")) { navigate(MoreDestination.Library) }
                SettingsLink("rufin-download-symbolic", translate("Downloads")) { navigate(MoreDestination.Downloads) }
                SettingsLink("rufin-network-hotspot-symbolic", translate("Connect")) { navigate(MoreDestination.Connect) }
                SettingsLink("rufin-phonelink-symbolic", translate("Controller")) { navigate(MoreDestination.Controller) }
                SettingsLink("rufin-drive-harddisk-symbolic", translate("Storage")) { navigate(MoreDestination.Storage) }
                SettingsLink("rufin-system-lock-screen-symbolic", translate("Privacy & Security")) { navigate(MoreDestination.Privacy) }
            }
        }
        MoreDestination.Library -> SourcesPage(model, onAddSource, onEditSource)
        MoreDestination.General, MoreDestination.Playback, MoreDestination.Integrations,
        MoreDestination.Privacy -> AppSettingsPage(visible, model, preferences, player, browse, onOutput, onAddSource, onEditSource, navigate)
        MoreDestination.Downloads -> DownloadsPage(model, player) { navigate(MoreDestination.DownloadSettings) }
        MoreDestination.DownloadSettings -> DownloadSettingsPage(model, preferences)
        MoreDestination.BackupSettings -> BackupSettingsPage(model, preferences)
        MoreDestination.OverviewSettings -> OverviewSettingsPage(preferences)
        MoreDestination.HomeSettings -> HomeSettingsPage(preferences)
        MoreDestination.Customization -> CustomizationSettingsPage(preferences, browse, player)
        MoreDestination.Theme -> ThemeSettingsPage(preferences)
        MoreDestination.Storage -> StorageSettingsPage(model, preferences)
        MoreDestination.Routes -> RouteSettingsPage(browse)
        MoreDestination.Equalizer -> PlayerSettingsContent(player, PlayerSettingsPage.EQUALIZER)
        MoreDestination.Lyrics -> PlayerSettingsContent(player, PlayerSettingsPage.LYRICS)
        MoreDestination.Visualizer -> PlayerSettingsContent(player, PlayerSettingsPage.VISUALIZER)
        MoreDestination.Controller -> ControllerPage(model)
        MoreDestination.Connect -> ConnectPage(model)
        MoreDestination.Overview -> ListeningOverviewPage(model, more, player)
        MoreDestination.Versions -> VersionHistoryPage(more)
        MoreDestination.Troubleshooting -> TroubleshootingPage(model, onSaveLog)
    }
    }
    }
}
