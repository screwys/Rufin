package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.R
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.ui.PagerTabIndicator
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.ArtworkReadyContent
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.MediaNavigationSheet
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.settingsTextFieldColors
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.blur
import androidx.compose.ui.platform.LocalDensity
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.semantics.Role
import androidx.compose.foundation.background
import androidx.compose.foundation.Image
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.*
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.core.AndroidLibrary
import io.github.screwys.rufin.core.AndroidSortField
import io.github.screwys.rufin.core.AndroidRatingState
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.roundToInt
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.setProgress

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BrowseSortSheet(browse: BrowseConnection, onDismiss: () -> Unit, page: BrowsePage? = browse.visiblePage) {
    val options = page?.options
    val selected = if (options?.sort.isNullOrEmpty()) page?.sortSelection?.id else options?.sort
    val descending = if (options?.sort.isNullOrEmpty()) page?.sortSelection?.descending ?: false else options?.descending ?: false
    val tabs = if (page?.supportsFavoriteFilter == true || page?.supportsDownloadedFilter == true)
        listOf("Filter", "Sort", "Display") else listOf("Sort", "Display")
    val pager = rememberPagerState { tabs.size }
    var visited by remember(tabs) { mutableStateOf(setOf(0)) }
    val settledPage by remember { derivedStateOf { pager.settledPage } }
    val selectedPage by remember { derivedStateOf { pager.targetPage } }
    LaunchedEffect(settledPage) { visited = visited + settledPage }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.85f)) {
            PrimaryTabRow(selectedTabIndex = selectedPage, indicator = { PagerTabIndicator(pager) }) {
                tabs.forEachIndexed { index, title ->
                    Tab(selectedPage == index, { scope.launch { if (reduceMotion) pager.scrollToPage(index) else pager.animateScrollToPage(index) } },
                        text = { Text(translate(title)) })
                }
            }
            HorizontalPager(pager, Modifier.fillMaxWidth().weight(1f), verticalAlignment = Alignment.Top,
                beyondViewportPageCount = tabs.size - 1) { tabIndex ->
                Box(Modifier.fillMaxSize()) {
                if (tabIndex in visited) {
                Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 12.dp)) {
                    if (tabs[tabIndex] == "Filter") {
                        if (page?.supportsFavoriteFilter == true && options != null) {
                            ListItem(headlineContent = { Text(translate("Favorites")) },
                                leadingContent = { Checkbox(options.favoritesOnly, null) },
                                modifier = Modifier.toggleable(options.favoritesOnly, role = Role.Checkbox) { options.favoritesOnly = it })
                        }
                        if (page?.supportsDownloadedFilter == true && options != null) {
                            ListItem(headlineContent = { Text(translate("Downloaded")) },
                                leadingContent = { Checkbox(options.downloadedOnly, null) },
                                modifier = Modifier.toggleable(options.downloadedOnly, role = Role.Checkbox) { options.downloadedOnly = it })
                        }
                    } else if (tabs[tabIndex] == "Sort") page?.sortFields.orEmpty().forEach { field ->
                        val isSelected = selected == field.id
                        Row(Modifier.fillMaxWidth().clickable {
                            page?.chooseSort(field.id, if (isSelected) !descending else descending)
                        }.semantics { this.selected = isSelected }.padding(horizontal = 8.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                            if (isSelected) RufinIcon(if (descending) "rufin-go-down-symbolic" else "rufin-go-up-symbolic", null,
                                Modifier.size(24.dp), MaterialTheme.colorScheme.primary)
                            else Spacer(Modifier.size(24.dp))
                            Text(field.title, style = MaterialTheme.typography.bodyLarge)
                        }
                    } else {
                        (options?.displaySettings ?: page?.displaySettings)?.let { display ->
                            DisplayChoices("Layout", display.layouts, display.layout) { layout ->
                                page?.chooseDisplay(layout, display.size, display.gridSpacing)
                            }
                            DisplayChoices("Size", display.sizes, display.size) { size ->
                                page?.chooseDisplay(display.layout, size, display.gridSpacing)
                            }
                            if (display.layout == "Grid") DisplayChoices("Grid spacing", display.gridSpacings, display.gridSpacing) { spacing ->
                                page?.chooseDisplay(display.layout, display.size, spacing)
                            }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
                }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DisplayChoices(label: String, choices: List<AndroidSortField>, selected: String, choose: (String) -> Unit) {
    Text(translate(label), Modifier.padding(top = 12.dp, bottom = 8.dp), style = MaterialTheme.typography.titleSmall)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        choices.forEach { option -> FilterChip(selected == option.id, { choose(option.id) }, label = { Text(option.title) }) }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BrowseMenuSheet(
    model: RufinConnection, browse: BrowseConnection, item: BrowseItem, player: PlayerConnection,
    onDismiss: () -> Unit, onPlaylist: () -> Unit, onMetadata: () -> Unit, onRemove: (() -> Unit)? = null,
    onNavigate: (String, String, String?) -> Unit,
    onPlayNow: ((Boolean) -> Unit)? = null,
) {
    val projected = favoriteItem(model, item)
    val playingUri by remember(player) { derivedStateOf { player.playback?.mediaUri } }
    val playbackControls by remember(player) { derivedStateOf {
        Triple(player.playback?.shuffle == true, player.playback?.autoDj == true, player.playback?.repeat)
    } }
    var pinned by remember(item.row.pin, browse.library) { mutableStateOf<Boolean?>(null) }
    var rename by remember { mutableStateOf(false) }
    var delete by remember { mutableStateOf(false) }
    var random by remember { mutableStateOf(false) }
    var navigation by remember(item.row.key) { mutableStateOf(false) }
    var rating by remember(item.row.key) { mutableStateOf<AndroidRatingState?>(null) }
    LaunchedEffect(item.row.kind, item.row.mediaUri, browse.library) {
        if (item.row.kind in setOf("track", "album", "album_header", "artist")) {
            try { rating = browse.library?.rating(item.row.kind, item.row.mediaUri, item.row.sourceId) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { model.runAction { throw error } }
        }
    }
    var downloaded by remember(item.row.key) { mutableStateOf<io.github.screwys.rufin.core.AndroidTargetDownloadStatus?>(null) }
    LaunchedEffect(item.row.key, browse.library, model.downloads.state?.revision) {
        if (item.row.kind in setOf("track", "album", "album_header", "artist", "genre", "mood", "playlist", "smart_playlist", "folder")) {
            try { downloaded = browse.library?.targetDownloadStatus(item.row.detailRoute, item.row.mediaUri, item.row.sourceId) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { model.runAction { throw error } }
        }
    }
    LaunchedEffect(item.row.pin, browse.library) {
        item.row.pin?.let { pin ->
            try { pinned = browse.library?.isPinned(pin) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { model.runAction { throw error } }
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Color.Transparent,
        contentColor = MaterialTheme.colorScheme.onSurface, contentWindowInsets = { WindowInsets(0, 0, 0, 0) },
        dragHandle = null, sheetState = rememberModalBottomSheetState()) {
        MediaMenuBackdrop(item.row.artworkIdentity, player) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).windowInsetsPadding(WindowInsets.safeDrawing).padding(top = 8.dp, bottom = 16.dp)) {
            BottomSheetDefaults.DragHandle(Modifier.align(Alignment.CenterHorizontally))
            if (item.row.kind == "track" && item.row.mediaUri == playingUri) {
                val (shuffle, autoDj, repeat) = playbackControls
                Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
                    BrowsePrimaryAction("rufin-shuffle-symbolic",
                        "Shuffle", Modifier.weight(1f), selected = shuffle, action = player::shuffle)
                    BrowsePrimaryAction(if (repeat == io.github.screwys.rufin.core.AndroidRepeatMode.ONE) "rufin-repeat-one-symbolic" else "rufin-repeat-symbolic",
                        when (repeat) { io.github.screwys.rufin.core.AndroidRepeatMode.ONE -> "Repeat one"; io.github.screwys.rufin.core.AndroidRepeatMode.ALL -> "Repeat all"; else -> "Repeat off" },
                        Modifier.weight(1f), selected = repeat != null && repeat != io.github.screwys.rufin.core.AndroidRepeatMode.OFF, action = player::repeat)
                    BrowsePrimaryAction("rufin-auto-dj-symbolic", "Auto DJ",
                        Modifier.weight(1f), selected = autoDj, action = player::autoDj)
                    BrowsePrimaryAction("rufin-random-symbolic", "Play random", Modifier.weight(1f), selected = false, action = { random = true })
                }
                MediaMenuDivider()
            }
            BrowseMenuHeader(projected, player,
                onFavorite = if (item.row.kind in setOf("track", "album", "album_header", "artist")) ({ browse.favorite(projected) }) else null)
            val playable = item.row.kind in setOf("track", "album", "album_header", "artist", "genre", "mood", "playlist", "smart_playlist", "folder")
            if (playable) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    BrowsePrimaryAction("rufin-media-playback-start-symbolic", "Play", Modifier.weight(1f),
                        shuffle = { onPlayNow?.invoke(true) ?: browse.play(item, "now", true); onDismiss() }) {
                            onPlayNow?.invoke(false) ?: browse.play(item); onDismiss()
                        }
                    BrowsePrimaryAction("rufin-mail-forward-symbolic", "Play Next", Modifier.weight(1f),
                        shuffle = { browse.play(item, "next", true); onDismiss() }) { browse.play(item, "next"); onDismiss() }
                    BrowsePrimaryAction("rufin-go-last-symbolic", "Play Later", Modifier.weight(1f),
                        shuffle = { browse.play(item, "append", true); onDismiss() }) { browse.play(item, "append"); onDismiss() }
                }
                val radioLabel = when (item.row.kind) {
                    "track" -> "Track radio"
                    "album", "album_header" -> "Album radio"
                    "artist" -> "Artist radio"
                    "genre" -> "Genre radio"
                    "playlist" -> "Playlist radio"
                    else -> null
                }
                radioLabel?.let { label ->
                    ListItem(headlineContent = { Text(translate(label)) },
                        leadingContent = { RufinIcon("rufin-audio-only-symbolic", null, Modifier.size(24.dp)) },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                        trailingContent = {
                        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        listOf("now" to "Play", "next" to "Play Next", "append" to "Play Later").forEach { (placement, title) ->
                            OutlinedIconButton({ browse.radio(item, placement); onDismiss() }, Modifier.size(40.dp), shape = RoundedCornerShape(8.dp)) {
                                RufinIcon(when (placement) { "now" -> "rufin-media-playback-start-symbolic"; "next" -> "rufin-mail-forward-symbolic"; else -> "rufin-go-last-symbolic" },
                                    translate(title), Modifier.size(20.dp))
                            }
                        }
                        }
                    })
                }
                BrowseMenuAction("rufin-playlists-symbolic", "Add to Playlist", onPlaylist)
                downloaded?.let { status ->
                    if (status.downloaded == 0UL || status.downloaded < status.total)
                        BrowseMenuAction("rufin-download-symbolic", "Download") { browse.download(item); onDismiss() }
                    if (status.downloaded > 0UL)
                        BrowseMenuAction("rufin-user-trash-symbolic", "Remove Downloads") { browse.download(item, true); onDismiss() }
                }
            }
            if (item.row.kind in setOf("track", "album", "album_header", "artist")) {
                BrowseMenuAction("rufin-external-link-compact-symbolic", "Go to") { navigation = true }
                BrowseMenuAction("rufin-document-edit-symbolic", "Edit metadata", onMetadata)
            }
            pinned?.let { isPinned -> BrowseMenuAction("rufin-bookmark-symbolic",
                if (isPinned) "Remove from Pins" else "Add to Pins") { browse.pin(item, !isPinned); onDismiss() } }
            onRemove?.let { BrowseMenuAction("rufin-user-trash-symbolic", "Remove", it) }
            if (item.row.kind == "playlist" && item.row.writable && item.row.detailRoute != null) {
                MediaMenuDivider()
                BrowseMenuAction("rufin-document-edit-symbolic", "Edit Playlist") { rename = true }
                BrowseMenuAction("rufin-user-trash-symbolic", "Delete Playlist") { delete = true }
            }
            rating?.takeIf { it.visible }?.let { current ->
                MediaMenuDivider()
                MenuRating(current) { value -> model.runAction {
                    withContext(Dispatchers.IO) { browse.library?.setRating(item.row.kind, item.row.mediaUri, value) }
                    onDismiss()
                } }
            }
        }
        }
    }
    if (navigation) MediaNavigationSheet(player.metadataLinks?.takeIf { it.mediaUri == item.row.mediaUri }, player, { route, title, source ->
        navigation = false; onDismiss(); onNavigate(route, title, source)
    }, { navigation = false }, mediaUri = item.row.mediaUri, originRoute = item.row.detailRoute)
    if (random) RandomPlaySheet(model, browse) { random = false; onDismiss() }
    if (rename) PlaylistNameSheet(model, item.row.title, { rename = false }) { name ->
        browse.library?.renamePlaylist(item.row.detailRoute!!, name, null)
        browse.refresh()
        rename = false
        onDismiss()
    }
    if (delete) AlertDialog(onDismissRequest = { delete = false },
        title = { Text(translate("Delete \"{name}\"?").replace("{name}", item.row.title)) },
        confirmButton = { TextButton({ model.runAction {
            browse.library?.deletePlaylist(item.row.detailRoute!!)
            browse.refresh()
            delete = false
            onDismiss()
        } }) { Text(translate("Delete")) } },
        dismissButton = { TextButton({ delete = false }) { Text(translate("Cancel")) } })
}

@Composable
internal fun BrowseMenuHeader(item: BrowseItem, player: PlayerConnection, onFavorite: (() -> Unit)? = null) {
    val subtitle = listOf(item.row.artist, item.row.album.takeIf { item.row.kind == "track" || it != item.row.title }.orEmpty())
        .filter(String::isNotBlank).joinToString(" · ").ifBlank { item.row.subtitle }
    CompactMediaMenuHeader(item.row.artworkIdentity, subtitle, item.row.favorite, player, onFavorite) {
        Text(item.row.title, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurface)
    }
}

@Composable
internal fun CompactMediaMenuHeader(identity: ByteArray?, subtitle: String, favorite: Boolean, player: PlayerConnection,
    onFavorite: (() -> Unit)?, title: @Composable () -> Unit,
) {
    val artwork = rememberArtwork(identity, player, with(LocalDensity.current) { 64.dp.roundToPx() })
    val shade = if (MaterialTheme.colorScheme.surface.luminance() < .5f) Color.Black else Color.White
    Surface(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        shape = RoundedCornerShape(12.dp), color = shade.copy(alpha = .18f),
        contentColor = MaterialTheme.colorScheme.onSurface) {
    Row(Modifier.fillMaxWidth().padding(12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Artwork(artwork, Modifier.size(64.dp).clip(RoundedCornerShape(8.dp)))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            title()
            if (subtitle.isNotBlank()) Text(subtitle, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (onFavorite != null) IconButton(onFavorite, Modifier.size(40.dp)) {
            RufinIcon(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", translate("Favorite"), Modifier.size(24.dp),
                if (favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        }
    }
    }
    MediaMenuDivider()
}

@Composable
internal fun MediaMenuDivider() {
    HorizontalDivider(Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        color = MaterialTheme.colorScheme.onSurface.copy(alpha = .12f))
}

@Composable
internal fun MediaMenuBackdrop(identity: ByteArray?, player: PlayerConnection, content: @Composable () -> Unit) {
    ArtworkReadyContent(key = identity?.toList(), modifier = Modifier.fillMaxWidth()) {
    val artwork = rememberArtwork(identity, player, with(LocalDensity.current) { 64.dp.roundToPx() })
    val surface = MaterialTheme.colorScheme.surface
    val overlay = if (surface.luminance() < .5f) Color.Black.copy(alpha = .675f) else Color.White.copy(alpha = .75f)
    Box(Modifier.fillMaxWidth().background(surface)) {
        artwork?.let { cover ->
            Image(cover.bitmap.asImageBitmap(), null, Modifier.matchParentSize().blur(85.dp),
                contentScale = ContentScale.Crop, colorFilter = ColorFilter.tint(overlay, BlendMode.SrcOver))
        }
        content()
    }
    }
}

@Composable
private fun MenuRating(state: AndroidRatingState, commit: (UByte?) -> Unit) {
    val step = if (state.halfStars) 1 else 2
    val stored = ((state.value?.toInt() ?: 0).coerceIn(0, 10) + step - 1) / step * step
    var preview by remember(state.value, step) { mutableStateOf<Int?>(null) }
    var width by remember { mutableIntStateOf(1) }
    fun ratingAt(x: Float): Int = ((x / width * 10 / step).roundToInt() * step).coerceIn(0, 10)
    val value = preview ?: stored
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Row(Modifier.weight(1f).height(40.dp).onSizeChanged { width = it.width }
            .semantics {
                contentDescription = translate("Rating")
                progressBarRangeInfo = ProgressBarRangeInfo(value.toFloat(), 0f..10f, if (state.halfStars) 9 else 4)
                setProgress { requested ->
                    val rating = ((requested / step).roundToInt() * step).coerceIn(0, 10)
                    commit(rating.takeIf { it > 0 }?.toUByte())
                    true
                }
            }
            .pointerInput(step) { detectTapGestures(onTap = { commit(ratingAt(it.x).takeIf { it > 0 }?.toUByte()) }) }
            .pointerInput(step) {
                detectHorizontalDragGestures(onDragStart = { preview = ratingAt(it.x) },
                    onHorizontalDrag = { change, _ -> preview = ratingAt(change.position.x); change.consume() },
                    onDragEnd = { preview?.let { commit(it.takeIf { it > 0 }?.toUByte()) }; preview = null },
                    onDragCancel = { preview = null })
            }, verticalAlignment = Alignment.CenterVertically) {
            repeat(5) { index ->
                val icon = when ((value - index * 2).coerceIn(0, 2)) {
                    0 -> "rufin-non-starred-symbolic"
                    1 -> "rufin-semi-starred-symbolic"
                    else -> "rufin-starred-symbolic"
                }
                Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    RufinIcon(icon, null, Modifier.size(24.dp), if (value > index * 2) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun BrowsePrimaryAction(icon: String, label: String, modifier: Modifier, shuffle: (() -> Unit)? = null,
    selected: Boolean? = null, action: () -> Unit) {
    Surface(modifier.heightIn(min = 76.dp).semantics { if (selected != null) this.selected = selected }
        .combinedClickable(onClick = action, onLongClick = shuffle,
        onLongClickLabel = if (shuffle == null) null else translate("{action} (Shuffle)").replace("{action}", translate(label))),
        shape = RoundedCornerShape(12.dp), color = androidx.compose.ui.graphics.Color.Transparent) {
        Column(Modifier.padding(horizontal = 4.dp, vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            RufinIcon(icon, null, Modifier.size(28.dp), if (selected == true) MaterialTheme.colorScheme.primary else LocalContentColor.current, glyph = true)
            Text(translate(label), style = MaterialTheme.typography.labelMedium, maxLines = 2)
            if (shuffle != null) Text(translate("(hold to shuffle)"), style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2)
        }
    }
}

@Composable
internal fun BrowseMenuAction(icon: String, label: String, action: () -> Unit) {
    ListItem(headlineContent = { Text(translate(label)) },
        leadingContent = { RufinIcon(icon, null, Modifier.size(24.dp)) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier.clickable(onClick = action))
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlaylistPickerSheet(model: RufinConnection, browse: BrowseConnection, item: BrowseItem, player: PlayerConnection, onDismiss: () -> Unit) {
    var filter by remember { mutableStateOf("") }
    var skipDuplicates by remember { mutableStateOf(true) }
    var create by remember { mutableStateOf(false) }
    val library = browse.library
    val route = browse.routes.firstOrNull { it.id == "Playlists" }?.route
    val pages = remember(library, route, filter) {
        Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
            maxSize = 192, enablePlaceholders = true)) { BrowseChoiceSource(library, route, filter) }.flow
    }.collectAsLazyPagingItems()
    val loadError = (pages.loadState.refresh as? LoadState.Error)?.error ?: (pages.loadState.append as? LoadState.Error)?.error
    LaunchedEffect(loadError) { loadError?.let { error -> model.runAction { throw error } } }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = MaterialTheme.colorScheme.surface,
        dragHandle = null, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Text(translate("Add to Playlist"), Modifier.padding(horizontal = 24.dp, vertical = 16.dp), style = MaterialTheme.typography.titleLarge)
        BrowseMenuHeader(item, player)
        OutlinedTextField(filter, { filter = it }, label = { Text(translate("Search playlists")) }, singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp))
        ListItem(headlineContent = { Text(translate("Don't duplicate")) },
            trailingContent = { Switch(skipDuplicates, { skipDuplicates = it }) })
        BrowseMenuAction("rufin-folder-new-symbolic", "New Playlist") { create = true }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            if (pages.itemCount == 0 && pages.loadState.refresh is LoadState.NotLoading) item {
                BrowseEmptyState(filter.isNotBlank()) { filter = "" }
            }
            items(pages.itemCount, key = pages.itemKey { it.key }) { index ->
                val playlist = pages[index]
                if (playlist == null) Spacer(Modifier.height(72.dp))
                else {
                    val artwork = rememberArtwork(playlist.artworkIdentity, player, with(LocalDensity.current) { 50.dp.roundToPx() })
                    ListItem(headlineContent = { BrowseTitle(playlist, player = player) },
                        supportingContent = { Text(playlist.subtitle, maxLines = 1) },
                        leadingContent = { Artwork(artwork, Modifier.size(50.dp).clip(RoundedCornerShape(8.dp))) },
                        modifier = Modifier.clickable {
                        model.runAction {
                            playlist.detailRoute?.let { destination ->
                                if (item.row.kind == "track") browse.library?.addToPlaylist(destination, item.row.mediaUri, skipDuplicates)
                                else item.row.detailRoute?.let { target -> browse.library?.addTargetToPlaylist(destination, target, skipDuplicates) }
                            }
                            browse.refresh()
                            onDismiss()
                        }
                    })
                }
            }
            if (pages.loadState.refresh is LoadState.Loading || pages.loadState.append is LoadState.Loading) item {
                Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
            loadError?.let { failure -> item {
                ErrorRow(failure, translate("Retry"), pages::retry)
            } }
        }
    }
    if (create) PlaylistCreateSheet(model, { create = false }) { name, sourceId, public ->
        if (item.row.kind == "track") browse.library?.createPlaylist(sourceId, name, listOf(item.row.mediaUri), public)
        else item.row.detailRoute?.let { target -> browse.library?.createTargetPlaylist(sourceId, name, target, public) }
        browse.refresh()
        create = false
        onDismiss()
    }
}

@Composable
internal fun PlaylistCreateSheet(
    model: RufinConnection,
    onDismiss: () -> Unit,
    initialCurrentSource: Boolean = true,
    save: suspend (String, String?, Boolean?) -> Unit,
) {
    val source = model.sources?.let { state -> state.sources.firstOrNull { it.id == state.selectedSourceId } }
    var name by remember { mutableStateOf("") }
    var currentSource by remember(source?.id) { mutableStateOf(initialCurrentSource && source != null) }
    var public by remember { mutableStateOf(false) }
    val supportsPublic = currentSource && source?.supportsPlaylistPublic == true
    AlertDialog(onDismissRequest = onDismiss,
        title = { Text(translate("New Playlist")) },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedTextField(name, { name = it }, label = { Text(translate("Playlist name")) }, singleLine = true,
                colors = settingsTextFieldColors(),
                modifier = Modifier.fillMaxWidth(), trailingIcon = {
                    IconToggleButton(currentSource, { currentSource = it }, enabled = source != null,
                        modifier = Modifier.semantics {
                            contentDescription = if (currentSource) source?.name.orEmpty() else "Rufin"
                        }) {
                        if (currentSource && source != null) SourceIcon(source.kind)
                        else Image(painterResource(R.drawable.app_icon), null, Modifier.size(24.dp))
                    }
                })
            if (supportsPublic) Row(Modifier.fillMaxWidth().toggleable(public, role = Role.Switch) { public = it },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Switch(public, null)
                Text(translate("Public"))
            }
        } },
        confirmButton = { TextButton({ model.runAction {
            save(name.trim(), source?.id?.takeIf { currentSource }, public.takeIf { supportsPublic })
        } }, enabled = name.isNotBlank() && model.pendingActions == 0) { Text(translate("Create")) } },
        dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PlaylistNameSheet(model: RufinConnection, initial: String, onDismiss: () -> Unit, save: suspend (String) -> Unit) {
    var name by remember { mutableStateOf(initial) }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = MaterialTheme.colorScheme.surface,
        dragHandle = null, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().imePadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate(if (initial.isEmpty()) "New Playlist" else "Edit Playlist"), style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(name, { name = it }, label = { Text(translate("Playlist name")) }, singleLine = true,
                modifier = Modifier.fillMaxWidth(), colors = settingsTextFieldColors())
            Button({ model.runAction { save(name) } }, enabled = name.isNotBlank() && model.pendingActions == 0) { Text(translate("Save")) }
        }
    }
}

internal class BrowseChoiceSource(
    private val library: AndroidLibrary?, private val route: String?, private val filter: String,
) : PagingSource<Int, AndroidBrowseRow>() {
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, AndroidBrowseRow> = try {
        val offset = params.key ?: 0
        if (library == null || route == null) LoadResult.Page(emptyList(), null, null)
        else {
            val query = library.browse(route, "tracks", filter, "Title", false, false, false)
            try {
                val count = query.count().toInt()
                val rows = query.page(offset.toULong(), params.loadSize.toUInt())
                LoadResult.Page(rows, if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                    if (offset + rows.size >= count) null else offset + rows.size,
                    itemsBefore = offset, itemsAfter = (count - offset - rows.size).coerceAtLeast(0))
            } finally { query.destroy() }
        }
    } catch (cancelled: CancellationException) { throw cancelled }
    catch (error: Exception) { LoadResult.Error(error) }
    override fun getRefreshKey(state: PagingState<Int, AndroidBrowseRow>): Int? = state.anchorPosition
}
