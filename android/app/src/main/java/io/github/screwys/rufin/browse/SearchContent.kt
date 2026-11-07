package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.library.SearchMode
import io.github.screwys.rufin.shared.rufin_core.RecentSearchKind
import io.github.screwys.rufin.shared.rufin_core.showsLyricsCachePrompt
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.LoadState
import androidx.paging.LoadStates
import androidx.paging.PagingData
import androidx.paging.compose.collectAsLazyPagingItems
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.shared.rufin_core.SourceOperation
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.rememberArtwork
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class)
@Composable
internal fun SearchContent(
    model: RufinConnection,
    preferences: PreferencesConnection,
    browse: BrowseConnection,
    player: PlayerConnection,
    onMenu: (BrowseItem) -> Unit,
    onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    var wasDownloadingAssets by remember { mutableStateOf(false) }
    LaunchedEffect(model.sourceOperation) {
        val downloading = model.sourceOperation is SourceOperation.Downloading
        if (wasDownloadingAssets && !downloading && browse.section == AppSection.Search &&
            browse.searchMode == SearchMode.LYRICS && browse.searchText.isNotBlank()) {
            browse.submitSearch()
        }
        wasDownloadingAssets = downloading
    }
    Column(Modifier.fillMaxSize()) {
        if (browse.searchText.isNotBlank()) {
            SingleChoiceSegmentedButtonRow(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
                listOf(SearchMode.TITLE to "By Title", SearchMode.LYRICS to "By Lyrics").forEachIndexed { index, (mode, label) ->
                    SegmentedButton(selected = browse.searchMode == mode, onClick = { browse.searchMode = mode },
                        shape = SegmentedButtonDefaults.itemShape(index, 2), icon = {},
                        colors = SegmentedButtonDefaults.colors(
                            activeContainerColor = MaterialTheme.colorScheme.primary,
                            activeContentColor = MaterialTheme.colorScheme.onPrimary,
                            activeBorderColor = MaterialTheme.colorScheme.primary,
                            inactiveContainerColor = Color.Transparent,
                            inactiveContentColor = MaterialTheme.colorScheme.primary,
                            inactiveBorderColor = MaterialTheme.colorScheme.primary)) {
                        Text(translate(label))
                    }
                }
            }
        }
        val settings = preferences.applicationSettings
        val showLyricsCachePrompt = browse.searchText.isNotBlank() &&
            browse.searchMode == SearchMode.LYRICS &&
            settings != null && showsLyricsCachePrompt(settings, model.sources?.sources.orEmpty())
        if (showLyricsCachePrompt) {
            Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                Surface(
                    modifier = Modifier.fillMaxWidth(),
                    shape = MaterialTheme.shapes.large,
                    color = MaterialTheme.colorScheme.surfaceContainer,
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, top = 52.dp, end = 16.dp, bottom = 16.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(
                            translate("Do you want to cache all lyrics automatically? You can also enable this from Preferences -> Storage"),
                            style = MaterialTheme.typography.bodyMedium,
                            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                        )
                        Button(onClick = {
                            preferences.changePreference { setDownloadLyricsAutomatically(true) }
                        }) {
                            Text(translate("Enable caching lyrics"))
                        }
                    }
                }
                IconButton(
                    onClick = {
                        preferences.changePreference { setLyricsCachePromptDismissed(true) }
                    },
                    modifier = Modifier.align(Alignment.TopEnd),
                ) {
                    RufinIcon("rufin-window-close-symbolic", translate("Close"))
                }
            }
        }
        Box(Modifier.weight(1f)) {
            SearchResults(browse, player, onMenu, onPlaylist, onScrolled)
        }
    }
}

@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class)
@Composable
private fun SearchResults(
    browse: BrowseConnection,
    player: PlayerConnection,
    onMenu: (BrowseItem) -> Unit,
    onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    if (browse.searchText.isNotBlank()) {
        // Search reads its rows whole: a result list is loaded, and an empty one shows as empty.
        val searchFlow = remember(browse) { snapshotFlow { browse.searchRows }.map { PagingData.from(it, LOADED) } }
        val rows = searchFlow.collectAsLazyPagingItems()
        BrowseContent(rows, emptyList(), player, false, false, false, browse::open, onMenu,
            { _, _ -> }, browse::refreshSection, browse::favorite, onPlaylist, onScrolled,
            loading = browse.loading, onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) })
        return
    }
    val state = rememberLazyListState()
    var selected by remember { mutableStateOf<RecentSearch?>(null) }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
            .distinctUntilChanged().collect(onScrolled)
    }
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(bottom = 8.dp + LocalPlayerBottomPadding.current)) {
        item {
            Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Recent searches"), Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                if (browse.recentSearchRows.isNotEmpty()) TextButton(browse::clearRecentSearches) { Text(translate("Clear")) }
            }
        }
        items(browse.recentSearchRows, key = { "${it.result.kind}:${it.result.mediaUri}" }) { recent ->
            // Drawn as saved, as GTK's search popup does; states, badge and menu come from its row.
            val (result, item) = recent.result to recent.item
            val pixels = with(LocalDensity.current) { 50.dp.roundToPx() }
            val artwork = rememberArtwork(result.artworkBinding, player, pixels)
            val row = @Composable {
            ListItem(
                headlineContent = { if (item != null) BrowseTitle(favoriteItem(player.model, item), player = player, title = result.title)
                    else Text(result.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                supportingContent = {
                    Text(listOf(result.subtitle, translate(when (result.kind) {
                        RecentSearchKind.ALBUM -> "Album"
                        RecentSearchKind.ARTIST -> "Artist"
                        RecentSearchKind.TRACK -> "Track"
                    })).filter(String::isNotBlank).joinToString(" · "), maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                leadingContent = { Artwork(artwork, Modifier.size(50.dp).clip(RoundedCornerShape(8.dp))) },
                trailingContent = {
                    if (item != null) PlayerIconButton("rufin-view-more-symbolic", "More", { selected = recent })
                    else PlayerIconButton("rufin-user-trash-symbolic", "Remove", { browse.removeRecentSearch(result) })
                },
                modifier = Modifier.combinedClickable(onClick = { browse.openRecent(recent) }, onLongClick = { selected = recent }),
            )
            }
            if (item != null) BrowseSwipeActions(item, true, row) else row()
        }
        if (browse.recentSearchRows.isEmpty()) item {
            Column(Modifier.fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                RufinIcon("rufin-search-symbolic", null, Modifier.size(32.dp), MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
    selected?.let { recent -> recent.item?.let { item ->
        BrowseMenuSheet(player.model, browse, item, player, { selected = null },
            { selected = null; onPlaylist(item) }, { metadata = item; selected = null },
            { browse.removeRecentSearch(recent.result); selected = null },
            onNavigate = { route, title, _ -> selected = null; browse.openRoute(route, title) })
    } }
    metadata?.row?.metadataItem?.let { item -> io.github.screwys.rufin.metadata.MetadataEditorScreen(player.model, item) { metadata = null } }
}

private val LOADED = LoadStates(LoadState.NotLoading(false), LoadState.NotLoading(true), LoadState.NotLoading(true))
