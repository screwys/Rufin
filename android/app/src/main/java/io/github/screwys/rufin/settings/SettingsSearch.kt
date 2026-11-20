package io.github.screwys.rufin.settings

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.player.PlaybackTab
import io.github.screwys.rufin.shared.rufin_core.DownloadRule
import io.github.screwys.rufin.shared.rufin_core.downloadRuleTitle
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.delay

/** A tab of a settings page, which a search result picks before it shows its row. */
internal interface SettingsTab { val title: String }

/** A setting Search finds, as GTK's preferences search lists it: its title, the page and group it is under, and
 *  the page, tab and row that open for it. */
internal class SettingsSearchItem(
    val destination: MoreDestination,
    val title: String,
    val context: String,
    val tab: SettingsTab?,
    /** The integration page that shows it, for one under Integrations. */
    val integration: IntegrationPage?,
    /** The title of the row or group that scrolls into view; a group's for an entry without a row of its own, and
     *  none where the page has neither. */
    val target: String?,
    summary: String,
) {
    val searchable = "$title $summary $context".lowercase()
}

/** The row a search result opened its page for. */
internal class SettingsTarget(val title: String?, val tab: SettingsTab?) {
    /** Whether a row with the title took it, so a second one does not flash too. */
    var claimed = false
}

@Stable
internal class SettingsHighlight {
    var target by mutableStateOf<SettingsTarget?>(null)
}

internal val LocalSettingsHighlight = compositionLocalOf<SettingsHighlight?> { null }

/** Shows the tab of type [T] a search result opened its page for. */
@Composable
internal inline fun <reified T : SettingsTab> SearchedTab(crossinline show: (T) -> Unit) {
    val target = LocalSettingsHighlight.current?.target
    LaunchedEffect(target) { (target?.tab as? T)?.let { show(it) } }
}

/** What a row or group titled [title] adds: once a search result opens its page for it, it scrolls into view and
 *  flashes in the accent, or holds it where motion is reduced. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun searchHighlight(title: String?): Modifier {
    val highlight = LocalSettingsHighlight.current ?: return Modifier
    val target = highlight.target?.takeIf { title != null && it.title == title } ?: return Modifier
    val requester = remember(target) { BringIntoViewRequester() }
    val flash = remember(target) { Animatable(0f) }
    val color = MaterialTheme.colorScheme.primary
    val reduceMotion = LocalReduceMotion.current
    LaunchedEffect(target) {
        if (target.claimed) return@LaunchedEffect
        target.claimed = true
        // Laid out first, so there is somewhere to scroll to.
        withFrameNanos { }
        requester.bringIntoView()
        if (reduceMotion) { flash.snapTo(.2f); delay(1200); flash.snapTo(0f) }
        else repeat(2) { flash.animateTo(.2f, tween(200)); flash.animateTo(0f, tween(400)) }
        if (highlight.target === target) highlight.target = null
    }
    return Modifier.bringIntoViewRequester(requester).drawWithContent {
        drawContent()
        drawRect(color, alpha = flash.value)
    }
}

/** As GTK's: every word of the query is somewhere in the title, the row's summary or where it is. */
internal fun settingsSearchMatches(searchable: String, query: String) =
    query.split(Regex("\\s+")).filter { it.isNotEmpty() }.all { searchable.contains(it.lowercase()) }

/** The settings Search finds. Compose has no tree of every page to read as GTK reads its UI files, so this lists
 *  each page's rows; a row added to a page belongs here too. */
internal fun settingsSearchItems(model: RufinConnection, preferences: PreferencesConnection, browse: BrowseConnection): List<SettingsSearchItem> {
    val items = mutableListOf<SettingsSearchItem>()
    fun page(destination: MoreDestination, tab: SettingsTab? = null, integration: IntegrationPage? = null, rows: PageRows.() -> Unit) =
        PageRows(items, destination, tab, integration).rows()

    page(MoreDestination.General) {
        rows(null, "Language")
        rows("App settings", "Starting page", "App icon", "Update check interval", "Allow permanently deleting tracks", "Listening overview")
        rows("Notifications", "Control notifications", "Release notifications")
        rows("Metadata", "Prefer distinct track covers", "Use sort tags")
    }
    page(MoreDestination.OverviewSettings) {
        rows("Monthly", "Open automatically", "Days before month ends", "Days after month ends")
        rows("Yearly", "Open automatically", "Days before year ends", "Days after year ends")
    }
    page(MoreDestination.Customization, CustomizationTab.Layout) {
        rows("Accessibility", "Reduce motion")
        rows("Grids", "Show a badge for favorited items", "Show star ratings")
        group("Library items")
        browse.routeSettings.forEach { row(it.descriptor.title, translate("Library items")) }
        rows("Swipe on a row", "Swipe right", "Swipe left")
    }
    page(MoreDestination.Customization, CustomizationTab.Home) {
        rows("Display", "Size", "Spacing")
        group("Blocks")
        preferences.applicationSettings?.let {
            runCatching { preferences.nativeSettings().homeBlockRows() }.getOrNull()?.forEach { block ->
                row(translate(block.title), translate("Blocks"))
            }
        }
        group("Mixes")
    }
    page(MoreDestination.Customization, CustomizationTab.Player) {
        rows("Seekbar", "Waveform seekbar")
        rows("Fullscreen player", "Show lyrics", "Show current lyrics line", "Show visualizer", "Combine lyrics and visualizer",
            "Dynamic background", "Enable background image")
    }
    page(MoreDestination.Customization, CustomizationTab.Menus) {
        // The menu's entries are its preview, with no rows: the group shows.
        val group = translate("Item menus")
        group("Item menus")
        (menuPlays.map { it.first } + menuActions.map { it.second } + "Rating").forEach { row(translate(it), group, target = group) }
    }
    page(MoreDestination.Theme) {
        row(translate("Accent color"), target = null)
    }
    page(MoreDestination.Integrations) {
        rows("Scrobbling", "Last.fm", "ListenBrainz", "Libre.fm", "Add server")
        rows("Lookups", "Metadata", "Lyrics")
        rows("Links", "Page links")
        rows("Remote folders", "WebDAV / Nextcloud", "SMB / Samba")
    }
    page(MoreDestination.Integration, integration = IntegrationPage("metadata", translate("Metadata"))) {
        row(translate("External metadata lookup"), summary = translate("Also album types and tag suggestions"))
        group("Album covers")
        group("Artist images")
        rows("fanart.tv key", "Personal API key")
    }
    page(MoreDestination.Integration, integration = IntegrationPage("lyrics", translate("Lyrics"))) {
        rows(null, "External lyrics lookup", "Prefer server lyrics")
        group("Providers")
    }
    page(MoreDestination.Integration, integration = IntegrationPage("links", translate("Page links"))) {
        rows(null, "Show external site links")
        rows("Services", "Last.fm", "MusicBrainz", "Server")
    }
    page(MoreDestination.Playback, PlaybackTab.Queue) {
        rows(null, "Transition mode", "Crossfade duration", "Skip same-album crossfade", "Playback speed", "Preserve pitch",
            "Audio fade on play/pause", "Skip unplayable tracks", "Clearing queue also clears the current song")
    }
    page(MoreDestination.Playback, PlaybackTab.AutoDj) {
        row(translate("Override server recommendations"), summary = translate("Play Rufin's picks instead of the server's own"))
        row(translate("Similar artists from ListenBrainz"), summary = translate("Look up the artists that listeners play alongside the current one"))
        rows(null, "Auto DJ refill threshold", "Tracks per refill", "Same artist")
        row(translate("Skip low-rated tracks"), summary = translate("Leave out tracks rated two stars or lower"))
    }
    page(MoreDestination.Playback, PlaybackTab.Audio) {
        rows(null, "Audio output", "Loudness normalization", "Normalization scope", "Target loudness", "Write EBU R128 tags to files", "Volume scale")
        group("Stream quality")
        group("Stream quality on mobile data")
    }
    page(MoreDestination.Equalizer) {
        rows(null, "Equalizer", "Automatic preamp", "Preamp")
        rows("Audio effects", "Bass", "Treble", "Balance")
    }
    page(MoreDestination.Library) {
        group("Sources")
        model.sources?.sources.orEmpty().filter { it.name.isNotBlank() }.forEach { row(it.name, translate("Sources")) }
        rows("Sources", "Add Source")
        group("Export Activity")
        group("Import Activity")
    }
    page(MoreDestination.DownloadSettings) {
        row(translate("Show downloaded badge"))
        row(translate("Download on Mobile Data"), summary = translate("Otherwise automatic downloads wait for Wi-Fi"))
        rows("Storage", "Download Folder")
        DownloadRule.entries.forEach { row(downloadRuleTitle(it)) }
        rows("Downloaded", "Remove All Downloads")
    }
    page(MoreDestination.Connect) {
        rows(null, "Device name", "Enable Rufin Connect", "Join profile", "Connect a device")
        rows("Connection settings", "Nearby discovery", "Relay address", "Sync folder")
    }
    page(MoreDestination.Controller) {
        rows(null, "Enable Controller", "Allow remote connections", "Port", "Access token")
        group("Controller status")
    }
    page(MoreDestination.Storage) {
        rows("Cache", "Artwork", "Lyrics")
        row(translate("Clear all cache"), target = null)
        row(translate("Clear all data"), target = null)
    }
    page(MoreDestination.Privacy) {
        rows(null, "Private mode", "Proxy casting through Rufin", "Casting network", "Secret storage")
        group("Backups")
        row(translate("Import Backup"), translate("Backups"), target = translate("Backups"))
        row(translate("Export Backup"), translate("Backups"), target = translate("Backups"))
    }
    page(MoreDestination.BackupSettings) {
        rows("Automatic Backups", "Enable automatic backups", "Backup Folder", "Number of backups", "Schedule", "Weekday", "Hour",
            "Encrypt backups")
    }
    return items
}

/** One page's entries; titles and groups given as their English messages are translated here. */
private class PageRows(private val items: MutableList<SettingsSearchItem>, private val destination: MoreDestination,
    private val tab: SettingsTab?, private val integration: IntegrationPage?) {
    // Where the page is, from the root's category: "Playback · Audio", "Integrations · Metadata · Album covers".
    private val place = listOfNotNull(
        translate(destination.category.title),
        integration?.name ?: translate(destination.title).takeIf { destination != destination.category },
        tab?.let { translate(it.title) })

    fun row(title: String, group: String? = null, summary: String = "", target: String? = title) {
        val context = (place + listOfNotNull(group?.takeIf { it.isNotEmpty() })).distinct().joinToString(" · ")
        items += SettingsSearchItem(destination, title, context, tab, integration, target, summary)
    }

    fun rows(group: String?, vararg titles: String) {
        val translated = group?.let(::translate)
        titles.forEach { row(translate(it), translated) }
    }

    /** A group whose rows are not found by name, such as a list of providers: its title finds it. */
    fun group(title: String) = row(translate(title))
}

/** The settings matching [query], each opening its page with [open]. */
@Composable
internal fun SettingsSearchResults(items: List<SettingsSearchItem>, query: String, open: (SettingsSearchItem) -> Unit) {
    val matches = remember(items, query) { items.filter { settingsSearchMatches(it.searchable, query) } }
    // Its rows share their titles with the rows they open, which flash instead while this page leaves.
    CompositionLocalProvider(LocalSettingsHighlight provides null) { SettingsPage {
        SettingsGroup {
            if (matches.isEmpty()) SettingsRow(translate("No results"))
            matches.forEach { item ->
                SettingsRow(item.title, summary = item.context,
                    leading = item.destination.category.icon?.let { { RufinIcon(it, null, Modifier.size(24.dp)) } },
                    trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) },
                    modifier = Modifier.clickable { open(item) })
            }
        }
    } }
}
