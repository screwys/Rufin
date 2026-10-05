package io.github.screwys.rufin.more

import io.github.screwys.rufin.settings.AppearanceSettingsPage
import io.github.screwys.rufin.settings.OverviewSettingsPage
import io.github.screwys.rufin.settings.BackupSettingsPage
import io.github.screwys.rufin.activity.ListeningOverviewPage
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerSettingsContent
import io.github.screwys.rufin.player.PlayerSettingsPage
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.tween
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.settings.RouteSettingsPage
import io.github.screwys.rufin.controller.ControllerPage
import io.github.screwys.rufin.connect.ConnectPage
import io.github.screwys.rufin.settings.SettingsIndexPage
import io.github.screwys.rufin.settings.LibrarySettingsPage
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
    Root("More"), Settings("Settings"), Overview("Listening overview"), OverviewSettings("Listening overview"),
    Versions("Version History"), Troubleshooting("Troubleshooting"),
    Appearance("Appearance"), Routes("Library"), Equalizer("Equalizer"), Lyrics("Lyrics"), Visualizer("Visualizer"),
    Controller("Controller"), Connect("Rufin Connect"), General("General"), Playback("Playback"), Library("Library"),
    Integrations("Integrations"), Privacy("Privacy and Security"), Downloads("Downloads"), DownloadSettings("Downloads"),
    BackupSettings("Backup Settings"), HomeSettings("Home Blocks");

    val parent get() = when (this) {
        Root -> null
        Routes, HomeSettings -> Appearance
        Equalizer, Lyrics, Visualizer -> Playback
        BackupSettings, OverviewSettings -> General
        General, Appearance, Playback, DownloadSettings, Privacy -> Settings
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
    val motionMillis = if (LocalReduceMotion.current) 0 else 220
    val destinations = rememberSaveableStateHolder()
    LaunchedEffect(destination, more.bridge) {
        more.overviewVisible = destination == MoreDestination.Overview
        if (more.overviewVisible && more.bridge != null) more.prepareOverview()
    }
    DisposableEffect(more) { onDispose { more.overviewVisible = false } }
    AnimatedContent(destination to navigationDepth, Modifier.fillMaxSize(), transitionSpec = {
        val direction = if (targetState.second > initialState.second) 1 else -1
        (slideInHorizontally(tween(motionMillis)) { it * direction } + fadeIn(tween(motionMillis / 2))) togetherWith
            (slideOutHorizontally(tween(motionMillis)) { -it * direction / 3 } + fadeOut(tween(motionMillis / 2)))
    }, label = "moreNavigation") { location ->
    val visible = location.first
    destinations.SaveableStateProvider(visible.name) {
    when (visible) {
        MoreDestination.Root -> SettingsPage {
            SettingsGroup {
                SettingsLink("rufin-library-symbolic", translate("Library")) { navigate(MoreDestination.Library) }
                SettingsLink("rufin-preferences-system-symbolic", translate("Settings")) { navigate(MoreDestination.Settings) }
                SettingsLink("rufin-download-symbolic", translate("Downloads")) { navigate(MoreDestination.Downloads) }
                SettingsLink("rufin-network-workgroup-symbolic", translate("Integrations")) { navigate(MoreDestination.Integrations) }
                SettingsLink("rufin-network-hotspot-symbolic", translate("Rufin Connect")) { navigate(MoreDestination.Connect) }
                SettingsLink("rufin-phonelink-symbolic", translate("Controller")) { navigate(MoreDestination.Controller) }
            }
            SettingsGroup {
                SettingsLink("rufin-activity-overview-symbolic", translate("Listening overview")) { navigate(MoreDestination.Overview) }
                SettingsLink("rufin-appointment-new-symbolic", translate("Version History")) { navigate(MoreDestination.Versions) }
                SettingsLink("rufin-utilities-terminal-symbolic", translate("Troubleshooting")) { navigate(MoreDestination.Troubleshooting) }
            }
        }
        MoreDestination.Settings -> SettingsIndexPage(navigate)
        MoreDestination.Library -> LibrarySettingsPage(model, preferences, onAddSource, onEditSource)
        MoreDestination.General, MoreDestination.Playback, MoreDestination.Integrations,
        MoreDestination.Privacy -> AppSettingsPage(visible, model, preferences, player, browse, onOutput, onAddSource, onEditSource, navigate)
        MoreDestination.Downloads -> DownloadsPage(model, player) { navigate(MoreDestination.DownloadSettings) }
        MoreDestination.DownloadSettings -> DownloadSettingsPage(model)
        MoreDestination.BackupSettings -> BackupSettingsPage(model, preferences)
        MoreDestination.OverviewSettings -> OverviewSettingsPage(preferences)
        MoreDestination.HomeSettings -> HomeSettingsPage(preferences)
        MoreDestination.Appearance -> AppearanceSettingsPage(preferences, browse)
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
