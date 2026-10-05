package io.github.screwys.rufin.browse

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.PagingData
import androidx.paging.compose.collectAsLazyPagingItems
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
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
    browse: BrowseConnection,
    player: PlayerConnection,
    onMenu: (BrowseItem) -> Unit,
    onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    if (browse.searchText.isNotBlank()) {
        val searchFlow = remember(browse) { snapshotFlow { browse.searchRows }.map { PagingData.from(it) } }
        val rows = searchFlow.collectAsLazyPagingItems()
        BrowseContent(rows, emptyList(), player, false, false, false, browse::open, onMenu,
            browse::playSection, browse::refreshSection, browse::favorite, onPlaylist, onScrolled,
            loading = browse.loading, onRadio = { browse.radio(it, "now") })
        return
    }
    val state = rememberLazyListState()
    var selected by remember { mutableStateOf<BrowseItem?>(null) }
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
        items(browse.recentSearchRows, key = { "${it.row.kind}:${it.row.mediaUri}" }) { item ->
            val pixels = with(LocalDensity.current) { 50.dp.roundToPx() }
            val artwork = rememberArtwork(item.row.artworkIdentity, player, pixels)
            BrowseSwipeActions(item, true) {
            ListItem(
                headlineContent = { BrowseTitle(item.row, player = player) },
                supportingContent = {
                    Text(listOf(item.row.subtitle, translate(when (item.row.kind) {
                        "album" -> "Album"
                        "artist" -> "Artist"
                        else -> "Track"
                    })).filter(String::isNotBlank).joinToString(" · "), maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                leadingContent = { Artwork(artwork, Modifier.size(50.dp).clip(RoundedCornerShape(8.dp))) },
                trailingContent = {
                    PlayerIconButton("rufin-view-more-symbolic", "More", { selected = item })
                },
                modifier = Modifier.combinedClickable(onClick = { browse.open(item) }, onLongClick = { selected = item }),
            )
            }
        }
        if (browse.recentSearchRows.isEmpty()) item {
            Column(Modifier.fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                RufinIcon("rufin-search-symbolic", null, Modifier.size(32.dp), MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
    selected?.let { item ->
        BrowseMenuSheet(player.model, browse, item, player, { selected = null },
            { selected = null; onPlaylist(item) }, { metadata = item; selected = null },
            { browse.removeRecentSearch(item); selected = null },
            onNavigate = { route, title, source -> selected = null; browse.openLinkedRoute(route, title, source) })
    }
    metadata?.let { item -> io.github.screwys.rufin.metadata.MetadataEditorScreen(player.model, item.row.kind, item.row.mediaUri) { metadata = null } }
}
