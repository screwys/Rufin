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
import io.github.screwys.rufin.settings.SettingsHighlight
import io.github.screwys.rufin.settings.SettingsSearchItem
import io.github.screwys.rufin.settings.SettingsSearchResults
import io.github.screwys.rufin.settings.SettingsTarget
import io.github.screwys.rufin.settings.LocalSettingsHighlight
import io.github.screwys.rufin.settings.settingsSearchItems
import kotlinx.coroutines.delay
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
    /** The page Settings' root lists that this one is under, or this one. */
    val category: MoreDestination get() = parent?.takeIf { it != Root }?.category ?: this
    /** The icon of a page on Settings' root. */
    val icon: String? get() = when (this) {
        General -> "rufin-preferences-system-symbolic"
        Customization -> "rufin-sliders-2-symbolic"
        Theme -> "rufin-preferences-desktop-appearance-symbolic"
        Integrations -> "rufin-network-workgroup-symbolic"
        Playback -> "rufin-music-queue-symbolic"
        Equalizer -> "rufin-sliders-vertical-symbolic"
        Library -> "rufin-library-symbolic"
        Downloads -> "rufin-download-symbolic"
        Connect -> "rufin-network-hotspot-symbolic"
        Controller -> "rufin-phonelink-symbolic"
        Storage -> "rufin-drive-harddisk-symbolic"
        Privacy -> "rufin-system-lock-screen-symbolic"
        else -> null
    }
}

@Composable
internal fun MoreContent(model: RufinConnection, preferences: PreferencesConnection, player: PlayerConnection, browse: BrowseConnection,
    destination: MoreDestination, navigate: (MoreDestination) -> Unit,
    /** The integration page Integration shows, and how one opens. */
    integration: IntegrationPage?, openIntegration: (IntegrationPage) -> Unit, back: () -> Unit,
    onAddSource: () -> Unit, onEditSource: (String) -> Unit, onOpen: () -> Unit, onSaveLog: () -> Unit, onOutput: () -> Unit,
    navigationDepth: Int,
    /** What Search on Settings' root looks for, while it is open there. */
    settingsQuery: String?,
    /** Opens the page a search result names; its row then shows itself. */
    openSearchResult: (SettingsSearchItem) -> Unit,
) {
    val more = model.more
    val highlight = remember { SettingsHighlight() }
    // A row the page no longer has, or one under a tab not shown, does not wait for a later visit.
    LaunchedEffect(highlight.target) {
        val target = highlight.target ?: return@LaunchedEffect
        delay(4000)
        if (highlight.target === target) highlight.target = null
    }
    // Read when Search opens, so it finds the sources and routes as they are then.
    val searchItems = remember(settingsQuery != null) {
        if (settingsQuery == null) emptyList() else settingsSearchItems(model, preferences, browse)
    }
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
    CompositionLocalProvider(LocalSettingsHighlight provides highlight) {
    destinations.SaveableStateProvider(visible.name) {
    when (visible) {
        MoreDestination.Root -> if (!settingsQuery.isNullOrBlank()) SettingsSearchResults(searchItems, settingsQuery) { item ->
            highlight.target = if (item.target != null || item.tab != null) SettingsTarget(item.target, item.tab) else null
            openSearchResult(item)
        } else SettingsPage {
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
                SettingsCategory(MoreDestination.General, navigate, listOfNotNull(language, start))
                SettingsCategory(MoreDestination.Customization, navigate)
                SettingsCategory(MoreDestination.Theme, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Integrations, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Playback, navigate, playback.orEmpty().filterNotNull())
                SettingsCategory(MoreDestination.Equalizer, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Library, navigate, model.sources?.sources.orEmpty().map { it.name }.filter { it.isNotBlank() }, ", ")
                SettingsCategory(MoreDestination.Downloads, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Connect, navigate)
                SettingsCategory(MoreDestination.Controller, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Storage, navigate)
                SettingsCategory(MoreDestination.Privacy, navigate)
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
}

/** A page of Settings on its root: its icon on a tonal tile, its name, and what is set there. */
@Composable
private fun SettingsCategory(page: MoreDestination, navigate: (MoreDestination) -> Unit, facts: List<String> = emptyList(),
    separator: String = " · ") {
    SettingsRow(translate(page.title), summary = facts.joinToString(separator).takeIf { it.isNotEmpty() }, leading = {
        Box(Modifier.size(36.dp).background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(12.dp)),
            contentAlignment = Alignment.Center) {
            RufinIcon(checkNotNull(page.icon), null, Modifier.size(20.dp), MaterialTheme.colorScheme.primary)
        }
    }, modifier = Modifier.clickable { navigate(page) })
}
