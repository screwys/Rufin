package io.github.screwys.rufin.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.compose.collectAsLazyPagingItems
import io.github.screwys.rufin.browse.AppSection
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.browse.BrowseItem
import io.github.screwys.rufin.browse.BrowsePage
import io.github.screwys.rufin.browse.subtitle
import io.github.screwys.rufin.browse.title
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.sources.SourceSelector
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.CoverShape
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.ui.rememberArtwork

/** What the sidebar opens: a route, a saved entry, a page of the ☰ menu, or the source pages. */
internal class SidebarActions(
    val route: (String) -> Unit,
    val saved: (BrowseItem) -> Unit,
    val more: (MoreDestination) -> Unit,
    val addSource: () -> Unit,
    /** The general search, where no bottom bar or rail offers it. */
    val search: (() -> Unit)? = null,
)

/** The full sidebar: the ☰ menu, the source, Home and the routes, and the Saved entries. */
@Composable
internal fun Sidebar(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection,
    saved: BrowsePage, actions: SidebarActions, modifier: Modifier = Modifier,
) {
    var menu by remember { mutableStateOf(false) }
    var sourceMenu by remember { mutableStateOf(false) }
    val rows = saved.pages.collectAsLazyPagingItems()
    LazyColumn(modifier.fillMaxHeight().widthIn(max = 320.dp), contentPadding = PaddingValues(start = 10.dp, end = 10.dp, top = 12.dp, bottom = 16.dp)) {
        item {
            Row(Modifier.fillMaxWidth().padding(start = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Library"), Modifier.weight(1f), style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                Box {
                    PlayerIconButton("rufin-open-menu-symbolic", "Menu", { menu = true })
                    DropdownMenu(menu, { menu = false }, Modifier.widthIn(min = 220.dp)) {
                        listOf(Triple(MoreDestination.Overview, "Listening overview", "rufin-activity-overview-symbolic"),
                            Triple(MoreDestination.Versions, "Version History", "rufin-appointment-new-symbolic"),
                            Triple(MoreDestination.Troubleshooting, "Troubleshooting", "rufin-utilities-terminal-symbolic"),
                        ).forEach { (destination, title, icon) ->
                            DropdownMenuItem(text = { Text(translate(title)) }, onClick = { menu = false; actions.more(destination) },
                                leadingIcon = { RufinIcon(icon, null, Modifier.size(20.dp)) })
                        }
                        DropdownMenuItem(text = { Text(translate("Settings")) }, onClick = { menu = false; actions.more(MoreDestination.Root) },
                            leadingIcon = { RufinIcon("rufin-preferences-system-symbolic", null, Modifier.size(20.dp)) })
                    }
                }
            }
        }
        item {
            // The source, as an entry like the routes under it; ▾ opens the source menu.
            val sources = model.sources
            val source = sources?.sources?.firstOrNull { it.id == sources.selectedSourceId }
            var rowWidth by remember { mutableStateOf(0.dp) }
            val density = LocalDensity.current
            Box(Modifier.onSizeChanged { rowWidth = with(density) { it.width.toDp() } }) {
                NavigationDrawerItem(label = { Text(source?.name ?: translate("Select Source"), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    selected = false, onClick = { sourceMenu = true },
                    icon = { if (source != null) SourceIcon(source.kind, Modifier.size(22.dp)) },
                    badge = { RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp)) }, modifier = Modifier.height(48.dp))
                SourceSelector(sourceMenu, model, browse, { sourceMenu = false },
                    manage = { actions.more(MoreDestination.Library) }, add = actions.addSource, width = rowWidth.takeIf { it > 0.dp })
            }
        }
        actions.search?.let { search -> item {
            val searching = browse.section == AppSection.Search
            NavigationDrawerItem(label = { Text(translate("Search")) }, selected = searching, onClick = search,
                icon = { RufinIcon("rufin-search-symbolic", null, Modifier.size(22.dp)) }, modifier = Modifier.height(48.dp))
        } }
        items(sidebarRoutes(browse), key = { it.id }) { route ->
            val selected = browse.section == AppSection.Browse && browse.rootId == route.id
            NavigationDrawerItem(label = { Text(route.title, maxLines = 1, overflow = TextOverflow.Ellipsis) }, selected = selected,
                onClick = { actions.route(route.id) },
                icon = { RufinIcon(if (selected) route.selectedIconName else route.iconName, null, Modifier.size(22.dp)) },
                modifier = Modifier.height(48.dp))
        }
        if (rows.itemCount > 0) item {
            Text(translate("Saved").uppercase(), Modifier.padding(start = 16.dp, top = 18.dp, bottom = 6.dp),
                style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        items(rows.itemCount) { index ->
            rows[index]?.let { item -> SavedRow(item, player) { actions.saved(item) } }
        }
    }
}

/** Home and the configured routes, as the sidebar and the rail list them; Search has its tab. */
internal fun sidebarRoutes(browse: BrowseConnection) = browse.configuredRoutes.filter { it.id != "Search" }

@Composable
private fun SavedRow(item: BrowseItem, player: PlayerConnection, open: () -> Unit) {
    val artwork = rememberArtwork(item.row.artworkIdentity, player, with(LocalDensity.current) { 36.dp.roundToPx() })
    Row(Modifier.fillMaxWidth().height(52.dp).clip(RoundedCornerShape(12.dp)).clickable(onClick = open).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Artwork(artwork, Modifier.size(36.dp).clip(CoverShape))
        Column(Modifier.weight(1f)) {
            Text(item.row.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium)
            CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
                io.github.screwys.rufin.browse.PinFacts(item, player.model)
            }
        }
    }
}

/** The compact icon sidebar of medium widths: ☰ opens the full one; then the routes, Search and Saved. */
@Composable
internal fun SidebarRail(browse: BrowseConnection, expand: () -> Unit, route: (String) -> Unit, search: () -> Unit, saved: () -> Unit) {
    // Icons, the selected one labelled; a long route list scrolls rather than crowding.
    NavigationRail(Modifier.fillMaxHeight(), header = { PlayerIconButton("rufin-open-menu-symbolic", "Expand sidebar", expand) }) {
        Column(Modifier.verticalScroll(rememberScrollState()), horizontalAlignment = Alignment.CenterHorizontally) {
            val searching = browse.section == AppSection.Search
            NavigationRailItem(searching, search, icon = { RufinIcon("rufin-search-symbolic", translate("Search"), Modifier.size(24.dp)) },
                label = { Text(translate("Search")) }, alwaysShowLabel = false, modifier = Modifier.semantics { selected = searching })
            sidebarRoutes(browse).forEach { item ->
                val selected = browse.section == AppSection.Browse && browse.rootId == item.id
                NavigationRailItem(selected, { route(item.id) },
                    icon = { RufinIcon(if (selected) item.selectedIconName else item.iconName, item.title, Modifier.size(24.dp)) },
                    label = { Text(item.title, maxLines = 1, overflow = TextOverflow.Ellipsis) }, alwaysShowLabel = false)
            }
            val showingSaved = browse.section == AppSection.Pins
            NavigationRailItem(showingSaved, saved, icon = { RufinIcon("rufin-bookmark-symbolic", translate("Saved"), Modifier.size(24.dp)) },
                label = { Text(translate("Saved")) }, alwaysShowLabel = false)
        }
    }
}
