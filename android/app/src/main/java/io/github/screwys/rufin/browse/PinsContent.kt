package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.LoadState
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.translate

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PinsContent(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection) {
    val scope = rememberCoroutineScope()
    val options = remember { BrowseOptions() }
    val location = remember { BrowseLocation("", "Pins", "Pins", options) }
    val page = remember(model, browse) { BrowsePage(model, browse, location, options, scope, pins = true) }
    LaunchedEffect(page) { page.observe() }
    SideEffect { browse.visiblePage = page }
    val rows = page.pages.collectAsLazyPagingItems()
    var action by remember { mutableStateOf<BrowseItem?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
        items(rows.itemCount, key = rows.itemKey { "${it.row.pin}:${it.index}" }) { index ->
            val item = rows[index]
            if (item == null) Spacer(Modifier.height(64.dp)) else {
                val artwork = rememberArtwork(item.row.artworkIdentity, player, with(LocalDensity.current) { 50.dp.roundToPx() })
                BrowseSwipeActions(item, true) {
                ListItem(headlineContent = { BrowseTitle(item.row, player = player) },
                    supportingContent = { if (item.row.subtitle.isNotBlank()) Text(item.row.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    leadingContent = {
                        Artwork(artwork, Modifier.size(50.dp).clip(RoundedCornerShape(8.dp)))
                    },
                    trailingContent = {
                        PlayerIconButton("rufin-view-more-symbolic", "More", { action = item })
                    },
                    modifier = Modifier.combinedClickable(onClick = { browse.open(item) }, onLongClick = { action = item }))
                }
            }
        }
        if (!page.loading && rows.itemCount == 0 && rows.loadState.refresh is LoadState.NotLoading) item {
            BrowseEmptyState()
        }
        if (page.loading || rows.loadState.refresh is LoadState.Loading || rows.loadState.append is LoadState.Loading) item {
            LinearProgressIndicator(Modifier.fillMaxWidth())
        }
        val failure = (rows.loadState.refresh as? LoadState.Error)?.error ?: (rows.loadState.append as? LoadState.Error)?.error
        failure?.let { error -> item { ErrorRow(error, translate("Retry"), rows::retry) } }
    }
    action?.let { item -> BrowseMenuSheet(model, browse, item, player, { action = null },
        { playlist = item; action = null }, { metadata = item; action = null },
        onNavigate = { route, title, source -> action = null; browse.openLinkedRoute(route, title, source) }) }
    playlist?.let { item -> PlaylistPickerSheet(model, browse, item, player) { playlist = null } }
    metadata?.let { item -> io.github.screwys.rufin.metadata.MetadataEditorScreen(model, item.row.kind, item.row.mediaUri) { metadata = null } }
}
