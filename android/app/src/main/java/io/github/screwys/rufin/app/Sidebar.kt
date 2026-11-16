package io.github.screwys.rufin.app

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import io.github.screwys.rufin.settings.LocalReduceMotion
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
import io.github.screwys.rufin.shared.rufin_core.SourceSummary
import io.github.screwys.rufin.core.AndroidMusicFolderState
import kotlinx.coroutines.CancellationException
import io.github.screwys.rufin.ui.CoverShape
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon

/** What the sidebar opens: a route, a saved entry, one of the app's own pages, or the source pages. */
internal class SidebarActions(
    val route: (String) -> Unit,
    val saved: (BrowseItem) -> Unit,
    val more: (MoreDestination) -> Unit,
    val addSource: () -> Unit,
    /** The general search, where no bottom bar or rail offers it. */
    val search: (() -> Unit)? = null,
)

/** The full sidebar: the source rail with the app's pages under it, and beside it the picked source as the
 *  title, which opens its server libraries, the general search where no bottom bar offers it, Home and the routes, and the Saved entries. */
@Composable
internal fun Sidebar(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection,
    saved: BrowsePage, actions: SidebarActions,
    /** The page of the rail's lower buttons that is open, which its button marks. */
    open: MoreDestination?, modifier: Modifier = Modifier,
    /** The player that a short, wide screen docks under the list. */
    dock: (@Composable () -> Unit)? = null,
    /** Folds a docked sidebar to its compact rail, where the screen offers both. */
    collapse: (() -> Unit)? = null,
) {
    Row(modifier.fillMaxHeight().widthIn(max = 326.dp)) {
        SourceRail(model, actions, open, collapse)
        Column(Modifier.weight(1f).fillMaxHeight()) {
            SidebarList(model, browse, player, saved, actions, Modifier.weight(1f))
            dock?.invoke()
        }
    }
}

/** The sources, one tap each, then Manage and Add; pinned under them the app's own pages. */
@Composable
private fun SourceRail(model: RufinConnection, actions: SidebarActions, open: MoreDestination?, collapse: (() -> Unit)?) {
    val scheme = MaterialTheme.colorScheme
    val sources = model.sources
    Column(Modifier.fillMaxHeight().width(66.dp).background(scheme.surfaceContainerLowest).padding(top = 12.dp, bottom = 12.dp),
        horizontalAlignment = Alignment.CenterHorizontally) {
        collapse?.let { PlayerIconButton("rufin-open-menu-symbolic", "Collapse sidebar", it) }
        LazyColumn(Modifier.weight(1f).fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(sources?.sources.orEmpty(), key = { it.id }) { source ->
                val selected = source.id == sources?.selectedSourceId
                SourceButton(source, selected) { if (!selected) model.selectSource(source.id) }
            }
            item {
                Surface({ actions.more(MoreDestination.Library) }, Modifier.size(44.dp), shape = CircleShape, color = scheme.surfaceContainer) {
                    Box(contentAlignment = Alignment.Center) {
                        RufinIcon("rufin-document-edit-symbolic", translate("Manage"), Modifier.size(20.dp), scheme.onSurfaceVariant)
                    }
                }
            }
            item {
                Surface(actions.addSource, Modifier.size(44.dp), shape = CircleShape, color = scheme.surfaceContainer) {
                    Box(contentAlignment = Alignment.Center) {
                        RufinIcon("rufin-list-add-symbolic", translate("Add a new source"), Modifier.size(20.dp), scheme.onSurfaceVariant)
                    }
                }
            }
        }
        RailPages(open, actions.more)
    }
}

/** The app's own pages, as both rails pin them at their bottom: plain buttons, the open one in the accent. */
@Composable
private fun RailPages(open: MoreDestination?, more: (MoreDestination) -> Unit) {
    val pages = listOf(Triple(MoreDestination.Overview, "Listening overview", "rufin-activity-overview-symbolic"),
        Triple(MoreDestination.Versions, "Version History", "rufin-appointment-new-symbolic"),
        Triple(MoreDestination.Troubleshooting, "Troubleshooting", "rufin-utilities-terminal-symbolic"),
        Triple(MoreDestination.Root, "Settings", "rufin-preferences-system-symbolic"))
    // Settings marks its own pages too, whichever of them is open.
    val picked = open?.let { page -> pages.firstOrNull { it.first == page }?.first ?: MoreDestination.Root }
    CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
        pages.forEach { (destination, title, icon) ->
            PlayerIconButton(icon, title, { more(destination) }, selected = picked == destination, iconSize = 20.dp)
        }
    }
}

/** A source of the rail: its logo over its name; the picked one squares off and takes an accent bar on its edge. */
@Composable
private fun SourceButton(source: SourceSummary, selected: Boolean, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Box(Modifier.fillMaxWidth().selectable(selected, role = Role.Tab, onClick = onClick)) {
        if (selected) Box(Modifier.align(Alignment.TopStart).padding(top = 8.dp).size(4.dp, 28.dp)
            .background(scheme.primary, RoundedCornerShape(topEnd = 4.dp, bottomEnd = 4.dp)))
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
            Box(Modifier.size(44.dp).background(if (selected) scheme.secondaryContainer else scheme.surfaceContainer,
                RoundedCornerShape(if (selected) 14.dp else 22.dp)), contentAlignment = Alignment.Center) {
                SourceIcon(source.kind, Modifier.size(24.dp))
            }
            Text(source.name, Modifier.padding(start = 3.dp, end = 3.dp, top = 3.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.labelSmall, fontWeight = if (selected) FontWeight.Bold else FontWeight.Medium,
                color = if (selected) scheme.onSurface else scheme.onSurfaceVariant)
        }
    }
}

/** The picked source as the list's title, its server library under the name; tapped, the source's libraries open
 *  under it and move the routes down. With no source the title is the app's name. */
@Composable
private fun LibraryTitle(source: String?, titles: List<String>, picked: Int, pick: (Int) -> Unit) {
    var open by remember(source) { mutableStateOf(false) }
    val scheme = MaterialTheme.colorScheme
    val duration = if (LocalReduceMotion.current) 0 else 180
    val turn by animateFloatAsState(if (open) 180f else 0f, tween(duration), label = "libraryTitle")
    Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
        Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp))
            .clickable(enabled = titles.isNotEmpty(), role = Role.Button) { open = !open }
            .padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(source ?: "Rufin", maxLines = 1, overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                titles.getOrNull(picked)?.let {
                    Text(it, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant)
                }
            }
            if (titles.isNotEmpty()) RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp).graphicsLayer { rotationZ = turn }, scheme.onSurfaceVariant)
        }
        AnimatedVisibility(open && titles.isNotEmpty(), enter = expandVertically(tween(duration)) + fadeIn(tween(duration)),
            exit = shrinkVertically(tween(duration)) + fadeOut(tween(duration))) {
            Column(Modifier.fillMaxWidth().padding(start = 2.dp, end = 2.dp, top = 2.dp, bottom = 8.dp).clip(RoundedCornerShape(18.dp)),
                verticalArrangement = Arrangement.spacedBy(3.dp)) {
                titles.forEachIndexed { index, title ->
                    val selected = index == picked
                    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(5.dp))
                        .background(if (selected) scheme.secondaryContainer else scheme.surfaceContainer)
                        .selectable(selected, role = Role.RadioButton) { open = false; if (!selected) pick(index) }
                        .heightIn(min = 46.dp).padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(title, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                        if (selected) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp), scheme.primary)
                    }
                }
            }
        }
    }
}

@Composable
private fun SidebarList(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection,
    saved: BrowsePage, actions: SidebarActions, modifier: Modifier) {
    val rows = saved.pages.collectAsLazyPagingItems()
    val phone = androidx.compose.ui.platform.LocalConfiguration.current.screenWidthDp < 600
    val sources = model.sources
    val selectedId = sources?.selectedSourceId
    var folders by remember(selectedId) { mutableStateOf<AndroidMusicFolderState?>(null) }
    LaunchedEffect(browse.library, sources, model.catalogChanges) {
        val library = browse.library ?: return@LaunchedEffect
        if (selectedId != model.sources?.selectedSourceId) return@LaunchedEffect
        try { folders = library.musicFolders() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { model.runAction { throw failure } }
    }
    LazyColumn(modifier.fillMaxWidth(), contentPadding = PaddingValues(start = 8.dp, end = 8.dp, top = 12.dp, bottom = 16.dp)) {
        actions.search?.let { search -> item {
            Surface(search, Modifier.fillMaxWidth().padding(bottom = 6.dp).height(44.dp), shape = RoundedCornerShape(22.dp),
                color = if (browse.section == AppSection.Search) MaterialTheme.colorScheme.secondaryContainer
                    else MaterialTheme.colorScheme.surfaceContainerHighest) {
                Row(Modifier.padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    RufinIcon("rufin-search-symbolic", null, Modifier.size(20.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                    Text(translate("Search"), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        } }
        item {
            val choices = folders?.choices.orEmpty().takeIf { list -> list.any { it.id != null } }.orEmpty()
            val source = sources?.sources?.firstOrNull { it.id == selectedId }
            LibraryTitle(source?.name, choices.map { it.title }, choices.indexOfFirst { it.id == folders?.selectedId }) { index ->
                model.runAction { browse.library?.setMusicFolder(choices[index].id) }
            }
        }
        // A phone has Radio on its bottom bar.
        items(sidebarRoutes(browse).filter { it.id != "Radio" || !phone }, key = { it.id }) { route ->
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
    Row(Modifier.fillMaxWidth().height(52.dp).clip(RoundedCornerShape(12.dp)).clickable(onClick = open).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        io.github.screwys.rufin.browse.PinCover(item, player, 36.dp)
        Column(Modifier.weight(1f)) {
            Text(item.row.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium)
            CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
                io.github.screwys.rufin.browse.PinFacts(item, player.model)
            }
        }
    }
}

/** The compact icon sidebar of foldable widths: ☰ opens the full one; then Search, Home and the routes, each
 *  with its name, and Saved with its entries' covers under it; the app's own pages are pinned at its bottom. */
@Composable
internal fun SidebarRail(browse: BrowseConnection, player: PlayerConnection, saved: BrowsePage, expand: () -> Unit,
    route: (String) -> Unit, search: () -> Unit, onSaved: () -> Unit, openSaved: (BrowseItem) -> Unit,
    /** The app's page that is open, and how one opens. */
    open: MoreDestination?, more: (MoreDestination) -> Unit) {
    val rows = saved.pages.collectAsLazyPagingItems()
    Surface(Modifier.fillMaxHeight().width(84.dp), color = MaterialTheme.colorScheme.surfaceContainerLow) {
        Column(Modifier.statusBarsPadding().padding(top = 4.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            PlayerIconButton("rufin-open-menu-symbolic", "Expand sidebar", expand)
            // A long list scrolls, and fades out at the bottom edge.
            LazyColumn(Modifier.weight(1f).fillMaxWidth()
                .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
                .drawWithContent {
                    drawContent()
                    drawRect(Brush.verticalGradient(0f to Color.Black, .88f to Color.Black, 1f to Color.Transparent), blendMode = BlendMode.DstIn)
                }, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp),
                contentPadding = PaddingValues(top = 6.dp, bottom = 40.dp)) {
                item { RailItem("rufin-search-symbolic", translate("Search"), browse.section == AppSection.Search, search) }
                items(sidebarRoutes(browse), key = { it.id }) { item ->
                    val selected = browse.section == AppSection.Browse && browse.rootId == item.id
                    RailItem(if (selected) item.selectedIconName else item.iconName, item.title, selected) { route(item.id) }
                }
                item { HorizontalDivider(Modifier.width(40.dp).padding(vertical = 2.dp)) }
                item { RailItem("rufin-bookmark-symbolic", translate("Saved"), browse.section == AppSection.Pins, onSaved) }
                items(rows.itemCount) { index ->
                    rows[index]?.let { item ->
                        Box(Modifier.clip(CoverShape).clickable(onClickLabel = item.row.title) { openSaved(item) }
                            .semantics { contentDescription = item.row.title }) {
                            io.github.screwys.rufin.browse.PinCover(item, player, 48.dp)
                        }
                    }
                }
            }
            Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                RailPages(open, more)
            }
        }
    }
}

@Composable
private fun RailItem(icon: String, label: String, selected: Boolean, onClick: () -> Unit) {
    Column(Modifier.width(84.dp).selectable(selected, role = Role.Tab, onClick = onClick), horizontalAlignment = Alignment.CenterHorizontally) {
        // The picked one takes the accent, as the desktop's compact sidebar does; no bubble.
        Box(Modifier.size(52.dp, 30.dp), contentAlignment = Alignment.Center) {
            RufinIcon(icon, null, Modifier.size(22.dp),
                if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(label, Modifier.padding(start = 4.dp, end = 4.dp, top = 3.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelSmall, fontWeight = if (selected) FontWeight.Bold else FontWeight.Medium,
            color = if (selected) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
