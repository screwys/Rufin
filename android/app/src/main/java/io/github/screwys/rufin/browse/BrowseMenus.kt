package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.ChoiceChip

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.rufin_core.createPlaylist
import io.github.screwys.rufin.shared.rufin_core.addPlaylistTracks
import io.github.screwys.rufin.shared.rufin_core.deletePlaylist
import io.github.screwys.rufin.shared.rufin_core.deletePlaylistFolder
import io.github.screwys.rufin.shared.rufin_core.movePlaylistToFolder
import io.github.screwys.rufin.shared.library.PlaylistFolderMember
import io.github.screwys.rufin.shared.library.RadioSeed
import io.github.screwys.rufin.shared.library.MemberSelection
import io.github.screwys.rufin.shared.library.FavoriteTarget
import io.github.screwys.rufin.shared.rufin_core.CollectionCategory
import io.github.screwys.rufin.shared.rufin_core.sourceHalfStarsEnabled
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.R
import io.github.screwys.rufin.ui.rufinMarquee
import io.github.screwys.rufin.ui.PagerTabIndicator
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.MediaNavigationSheet
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.settingsTextFieldColors
import androidx.compose.foundation.shape.RoundedCornerShape
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.blur
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalConfiguration
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
import io.github.screwys.rufin.core.AndroidOpened
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
    val selected = options?.sort ?: page?.sortSelection?.sortKey
    val descending = if (options?.sort == null) page?.sortSelection?.descending ?: false else options.descending
    val tabs = if (page?.supportsFavoriteFilter == true || page?.supportsDownloadedFilter == true)
        listOf("Filter", "Sort", "Display") else listOf("Sort", "Display")
    val pager = rememberPagerState { tabs.size }
    val selectedPage by remember { derivedStateOf { pager.targetPage } }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        // As Mihon's: the sheet is as tall as the tab it shows, up to most of the screen.
        Column(Modifier.fillMaxWidth().heightIn(max = LocalConfiguration.current.screenHeightDp.dp * .85f)) {
            PrimaryTabRow(selectedTabIndex = selectedPage, indicator = { PagerTabIndicator(pager) }) {
                tabs.forEachIndexed { index, title ->
                    Tab(selectedPage == index, { scope.launch { if (reduceMotion) pager.scrollToPage(index) else pager.animateScrollToPage(index) } },
                        text = { Text(translate(title)) })
                }
            }
            HorizontalPager(pager, Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) { tabIndex ->
                Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 12.dp)) {
                    if (tabs[tabIndex] == "Filter") {
                        if (page?.supportsFavoriteFilter == true && options != null) {
                            ListItem(colors = ListItemDefaults.colors(containerColor = Color.Transparent), headlineContent = { Text(translate("Favorites")) },
                                leadingContent = { Checkbox(options.favoritesOnly, null) },
                                modifier = Modifier.toggleable(options.favoritesOnly, role = Role.Checkbox) { options.favoritesOnly = it })
                        }
                        if (page?.supportsDownloadedFilter == true && options != null) {
                            ListItem(colors = ListItemDefaults.colors(containerColor = Color.Transparent), headlineContent = { Text(translate("Downloaded")) },
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
                            DisplayChoices("Layout", display.layouts, display.layout, { translate(when (it) {
                                LibraryLayout.ROW -> "Rows"; LibraryLayout.GRID -> "Grid"; LibraryLayout.DETAIL -> "Detail"
                            }) }) { layout ->
                                page?.chooseDisplay(layout, display.size, display.gridSpacing)
                            }
                            DisplayChoices("Size", DisplaySize.entries, display.size, { translate(when (it) {
                                DisplaySize.COMPACT -> "Compact"; DisplaySize.DEFAULT -> "Default"; DisplaySize.LARGE -> "Large"
                            }) }) { size ->
                                page?.chooseDisplay(display.layout, size, display.gridSpacing)
                            }
                            if (display.layout == LibraryLayout.GRID) DisplayChoices("Grid spacing", DisplaySize.entries, display.gridSpacing, { translate(when (it) {
                                DisplaySize.COMPACT -> "Small"; DisplaySize.DEFAULT -> "Default"; DisplaySize.LARGE -> "Wide"
                            }) }) { spacing ->
                                page?.chooseDisplay(display.layout, display.size, spacing)
                            }
                            if (display.fieldChoices.isNotEmpty()) {
                                Text(display.fieldsTitle, Modifier.padding(top = 12.dp, bottom = 4.dp), style = MaterialTheme.typography.titleSmall)
                                display.fieldChoices.forEach { field ->
                                    ListItem(colors = ListItemDefaults.colors(containerColor = Color.Transparent), headlineContent = { Text(field.title) },
                                        leadingContent = { Checkbox(field.shown, null, enabled = field.canToggle) },
                                        trailingContent = { Row {
                                            PlayerIconButton("rufin-go-up-symbolic", "Move up", { page?.moveField(field.id, true) }, enabled = field.shown)
                                            PlayerIconButton("rufin-go-down-symbolic", "Move down", { page?.moveField(field.id, false) }, enabled = field.shown)
                                        } },
                                        modifier = Modifier.toggleable(field.shown, field.canToggle, Role.Checkbox) { page?.showField(field.id, it) })
                                }
                            }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun <T> DisplayChoices(label: String, choices: List<T>, selected: T, title: (T) -> String, choose: (T) -> Unit) {
    Text(translate(label), Modifier.padding(top = 12.dp, bottom = 8.dp), style = MaterialTheme.typography.titleSmall)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        choices.forEach { option -> ChoiceChip(selected == option, { choose(option) }, label = { Text(title(option)) }) }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BrowseMenuSheet(
    model: RufinConnection, browse: BrowseConnection, listed: BrowseItem, player: PlayerConnection,
    onDismiss: () -> Unit, onPlaylist: () -> Unit, onMetadata: () -> Unit, onRemove: (() -> Unit)? = null,
    onNavigate: (Route, String, String?) -> Unit,
    onPlayNow: ((Boolean) -> Unit)? = null,
) {
    // The menu offers and labels what the row shows now, with the changes committed since.
    val item = favoriteItem(model, listed)
    var rename by remember { mutableStateOf(false) }
    var delete by remember { mutableStateOf(false) }
    var folderPicker by remember { mutableStateOf(false) }
    val playlistFolder = item.row.playlistFolder
    val smartPlaylist = item.row.smartPlaylist
    val folderMember = item.row.playlistFolderMember
    // What the menu offers, as GTK's menus do, under the saved menu settings.
    val menu = remember(item.row, browse.library) { browse.library?.menu(item.row) }
    var navigation by remember(item.row.key) { mutableStateOf(false) }
    var details by remember(item.row.key) { mutableStateOf(false) }
    var rating by remember(item.row.row) { mutableStateOf(item.row.rating) }
    val halfStars = model.sources?.let { sourceHalfStarsEnabled(it, item.row.mediaUri, item.row.sourceId) } == true
    if (details) {
        MediaDetailsDialog(model, browse, item, player) { details = false }
        return
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Color.Transparent,
        contentColor = MaterialTheme.colorScheme.onSurface, contentWindowInsets = { WindowInsets(0, 0, 0, 0) },
        // Short screens (a phone on its side) open the menu at full height.
        dragHandle = null, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = LocalConfiguration.current.screenHeightDp < 600)) {
        MediaMenuBackdrop(item.row.artworkIdentity, player) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).windowInsetsPadding(WindowInsets.safeDrawing).padding(top = 8.dp, bottom = 16.dp)) {
            BottomSheetDefaults.DragHandle(Modifier.align(Alignment.CenterHorizontally))
            val offered = menu
            BrowseMenuHeader(item, player)
            if (offered != null) {
                val plays = listOfNotNull(
                    Triple("rufin-media-playback-start-symbolic", "Play", null as QueuePlacement?).takeIf { offered.play },
                    Triple("rufin-mail-forward-symbolic", "Play Next", QueuePlacement.AfterCurrent).takeIf { offered.playNext },
                    Triple("rufin-go-last-symbolic", "Play Later", QueuePlacement.End).takeIf { offered.playLater })
                if (plays.isNotEmpty()) Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    plays.forEach { (icon, title, placement) ->
                        if (placement == null) BrowsePrimaryAction(icon, title, Modifier.weight(1f),
                            shuffle = { onPlayNow?.invoke(true) ?: browse.play(item, QueuePlacement.Replace(0UL), true); onDismiss() }) {
                                onPlayNow?.invoke(false) ?: browse.play(item); onDismiss()
                            }
                        else BrowsePrimaryAction(icon, title, Modifier.weight(1f),
                            shuffle = { browse.play(item, placement, true); onDismiss() }) { browse.play(item, placement); onDismiss() }
                    }
                }
                offered.radio?.let { seed ->
                    val label = when (seed) {
                        is RadioSeed.Track -> "Track radio"
                        is RadioSeed.Album -> "Album radio"
                        is RadioSeed.Artist, is RadioSeed.AlbumArtist -> "Artist radio"
                        is RadioSeed.Genre -> "Genre radio"
                        is RadioSeed.Playlist -> "Playlist radio"
                    }
                    ListItem(headlineContent = { Text(translate(label)) },
                        leadingContent = { RufinIcon("rufin-audio-only-symbolic", null, Modifier.size(24.dp)) },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                        trailingContent = {
                        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        listOf(Triple(QueuePlacement.Replace(0UL), "Play", "rufin-media-playback-start-symbolic"), Triple(QueuePlacement.AfterCurrent, "Play Next", "rufin-mail-forward-symbolic"),
                            Triple(QueuePlacement.End, "Play Later", "rufin-go-last-symbolic")).forEach { (placement, title, icon) ->
                            FilledTonalIconButton({ browse.radio(item, placement); onDismiss() }, Modifier.size(40.dp), shape = RoundedCornerShape(8.dp)) {
                                RufinIcon(icon, translate(title), Modifier.size(20.dp))
                            }
                        }
                        }
                    })
                }
                if (offered.addToPlaylist) BrowseMenuAction("rufin-playlists-symbolic", "Add to Playlist", onPlaylist)
                if (offered.favorite) BrowseMenuAction(if (item.row.favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                    if (item.row.favorite) "Remove from Favorites" else "Add to Favorites") { browse.favorite(item); onDismiss() }
                offered.folder?.let { inFolder ->
                    BrowseMenuAction("rufin-folders-symbolic", "Add to folder") { folderPicker = true }
                    if (inFolder && folderMember != null) BrowseMenuAction("rufin-list-remove-symbolic", "Remove from folder") { model.runAction {
                        val source = model.sourceOwner ?: return@runAction
                        movePlaylistToFolder(source, folderMember, null)
                        onDismiss()
                    } }
                }
                if (offered.removeUnavailable) BrowseMenuAction("rufin-user-trash-symbolic", "Remove unavailable tracks") { model.runAction {
                    browse.library?.removeUnavailableTracks(item.row.playlist!!.playlistKey)
                    onDismiss()
                } }
                if (offered.download) BrowseMenuAction("rufin-download-symbolic", "Download") { browse.download(item); onDismiss() }
                if (offered.removeDownload) BrowseMenuAction("rufin-user-trash-symbolic", "Remove Downloads") { browse.download(item, true); onDismiss() }
                if (offered.goTo) BrowseMenuAction("rufin-external-link-compact-symbolic", "Go to") { navigation = true }
                if (offered.editMetadata) BrowseMenuAction("rufin-document-edit-symbolic", "Edit metadata", onMetadata)
                onRemove?.let { BrowseMenuAction("rufin-user-trash-symbolic", "Remove", it) }
                if (playlistFolder != null) {
                    if (offered.edit) BrowseMenuAction("rufin-document-edit-symbolic", "Rename folder") { rename = true }
                    if (offered.delete) BrowseMenuAction("rufin-user-trash-symbolic", "Delete folder") { delete = true }
                }
                if (smartPlaylist != null) {
                    if (offered.edit) BrowseMenuAction("rufin-document-edit-symbolic", "Edit Smart Playlist") { rename = true }
                    if (offered.delete) BrowseMenuAction("rufin-user-trash-symbolic", "Delete Playlist") { delete = true }
                }
                if (item.row.playlist != null && (offered.edit || offered.delete)) {
                    MediaMenuDivider()
                    if (offered.edit) BrowseMenuAction("rufin-document-edit-symbolic", "Edit Playlist") { rename = true }
                    if (offered.delete) BrowseMenuAction("rufin-user-trash-symbolic", "Delete Playlist") { delete = true }
                }
                // Details and the pin close the menu, just above the rating.
                if (item.row.target != null) BrowseMenuAction("rufin-info-symbolic", "Details") { details = true }
                if (offered.pin != null) BrowseMenuAction("rufin-bookmark-symbolic",
                    if (offered.pinned) "Remove from Saved" else "Add to Saved") { browse.pin(item, !offered.pinned); onDismiss() }
                if (offered.rating) {
                    MediaMenuDivider()
                    MenuRating(rating, halfStars) { value -> model.runAction {
                        withContext(Dispatchers.IO) { item.row.target?.let { model.sourceOwner?.setRating(it, value) } }
                        onDismiss()
                    } }
                }
            }
        }
        }
    }
    if (navigation) MediaNavigationSheet(player.metadataLinks?.takeIf { it.mediaUri == item.row.mediaUri }, player, { route, title, source ->
        navigation = false; onDismiss(); onNavigate(route, title, source)
    }, { navigation = false }, mediaUri = item.row.mediaUri, originRoute = item.row.route)
    if (folderPicker) PlaylistPickerSheet(model, browse, item, player,
        folderKind = folderMember is PlaylistFolderMember.SmartPlaylist) { folderPicker = false; onDismiss() }
    if (rename) {
        if (playlistFolder != null) PlaylistFolderNameDialog(model, playlistFolder.smart,
            { rename = false; onDismiss() }, playlistFolder)
        else if (smartPlaylist != null) SmartPlaylistEditor(model, browse, { rename = false; onDismiss() }, smartPlaylist)
        else browse.library?.let { library ->
            PlaylistEditor(model, library, player, (item.row.route as Route.PlaylistDetail).v1, item.row.title,
                onDismiss = { rename = false; onDismiss() })
        }
    }
    if (delete) AlertDialog(onDismissRequest = { delete = false },
        title = { Text(translate("Delete \"{name}\"?").replace("{name}", item.row.title)) },
        confirmButton = { TextButton({ model.runAction {
            if (playlistFolder != null) {
                val source = model.sourceOwner ?: return@runAction
                deletePlaylistFolder(source, playlistFolder.folderKey)
                if ((browse.current?.route as? Route.PlaylistFolder)?.key == playlistFolder.folderKey) browse.back()
            } else if (smartPlaylist != null) browse.library?.deleteSmartPlaylist(smartPlaylist.smartPlaylistKey)
            else model.connectedService().runtime.value!!.getOrThrow().source().use { source ->
                deletePlaylist(source, (item.row.route as Route.PlaylistDetail).v1)
            }
            delete = false
            onDismiss()
        } }) { Text(translate("Delete")) } },
        dismissButton = { TextButton({ delete = false }) { Text(translate("Cancel")) } })
}

@Composable
internal fun BrowseMenuHeader(item: BrowseItem, player: PlayerConnection) {
    val subtitle = listOf(item.row.artist, item.row.album.takeIf { item.row.isTrack || it != item.row.title }.orEmpty())
        .filter(String::isNotBlank).joinToString(" · ").ifBlank { item.row.subtitle }
    CompactMediaMenuHeader(item.row.artworkIdentity, subtitle, player) {
        Text(item.row.title, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurface)
    }
}

@Composable
internal fun CompactMediaMenuHeader(identity: ByteArray?, subtitle: String, player: PlayerConnection, title: @Composable () -> Unit) {
    val artwork = rememberArtwork(identity, player, with(LocalDensity.current) { 64.dp.roundToPx() })
    val shade = if (MaterialTheme.colorScheme.surface.luminance() < .5f) Color.Black else Color.White
    Surface(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        shape = RoundedCornerShape(12.dp), color = shade.copy(alpha = .18f),
        contentColor = MaterialTheme.colorScheme.onSurface) {
    Row(Modifier.fillMaxWidth().padding(12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Artwork(artwork, Modifier.size(64.dp).clip(CoverShape))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            title()
            if (subtitle.isNotBlank()) Text(subtitle, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
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

@Composable
private fun MenuRating(value: UByte?, halfStars: Boolean, commit: (UByte?) -> Unit) {
    val step = if (halfStars) 1 else 2
    val stored = ((value?.toInt() ?: 0).coerceIn(0, 10) + step - 1) / step * step
    var preview by remember(value, step) { mutableStateOf<Int?>(null) }
    var width by remember { mutableIntStateOf(1) }
    fun ratingAt(x: Float): Int = ((x / width * 10 / step).roundToInt() * step).coerceIn(0, 10)
    val displayed = preview ?: stored
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Row(Modifier.weight(1f).height(40.dp).onSizeChanged { width = it.width }
            .semantics {
                contentDescription = translate("Rating")
                progressBarRangeInfo = ProgressBarRangeInfo(displayed.toFloat(), 0f..10f, if (halfStars) 9 else 4)
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
                val icon = when ((displayed - index * 2).coerceIn(0, 2)) {
                    0 -> "rufin-non-starred-symbolic"
                    1 -> "rufin-semi-starred-symbolic"
                    else -> "rufin-starred-symbolic"
                }
                Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    RufinIcon(icon, null, Modifier.size(24.dp), if (displayed > index * 2) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
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
internal fun PlaylistPickerSheet(
    model: RufinConnection, browse: BrowseConnection, item: BrowseItem, player: PlayerConnection,
    folderKind: Boolean? = null,
    /** The tracks to add instead of `item`'s: a Select mode's picked rows. */
    selectionOf: (() -> io.github.screwys.rufin.shared.library.MemberSelection?)? = null,
    onDismiss: () -> Unit,
) {
    var filter by remember { mutableStateOf("") }
    var skipDuplicates by remember { mutableStateOf(true) }
    var create by remember { mutableStateOf(false) }
    val library = browse.library
    val pages = remember(library, filter, folderKind) {
        Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
            maxSize = 192, enablePlaceholders = true)) { BrowseChoiceSource(library, Route.Playlists, filter, folderKind) }.flow
    }.collectAsLazyPagingItems()
    val loadError = (pages.loadState.refresh as? LoadState.Error)?.error ?: (pages.loadState.append as? LoadState.Error)?.error
    LaunchedEffect(loadError) { loadError?.let { error -> model.runAction { throw error } } }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = MaterialTheme.colorScheme.surface,
        dragHandle = null, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Text(translate(if (folderKind == null) "Add to Playlist" else "Add to folder"),
            Modifier.padding(horizontal = 24.dp, vertical = 16.dp), style = MaterialTheme.typography.titleLarge)
        BrowseMenuHeader(item, player)
        OutlinedTextField(filter, { filter = it }, label = { Text(translate(if (folderKind == null) "Search playlists" else "Search")) }, singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp))
        if (folderKind == null) ListItem(headlineContent = { Text(translate("Don't duplicate")) },
            trailingContent = { Switch(skipDuplicates, { skipDuplicates = it }) })
        BrowseMenuAction("rufin-list-add-symbolic", if (folderKind == null) "New Playlist" else "New folder") { create = true }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            if (pages.itemCount == 0 && pages.loadState.refresh is LoadState.NotLoading) item {
                BrowseEmptyState(filter.isNotBlank()) { filter = "" }
            }
            items(pages.itemCount, key = pages.itemKey { it.row.key }) { index ->
                val choice = pages[index]
                if (choice == null) Spacer(Modifier.height(72.dp))
                else {
                    val playlist = choice.row
                    val artwork = rememberArtwork(playlist.artworkIdentity, player, with(LocalDensity.current) { 50.dp.roundToPx() })
                    ListItem(headlineContent = { BrowseTitle(favoriteItem(model, choice), player = player) },
                        supportingContent = { Text(playlist.subtitle, maxLines = 1) },
                        leadingContent = { Artwork(artwork, Modifier.size(50.dp).clip(CoverShape)) },
                        modifier = Modifier.clickable {
                        model.runAction {
                            if (folderKind != null) {
                                val source = model.sourceOwner ?: return@runAction
                                movePlaylistToFolder(source, item.row.playlistFolderMember!!,
                                    playlist.playlistFolder!!.folderKey)
                            } else playlist.route?.let { destination ->
                                browse.library?.let { library ->
                                    (selectionOf?.invoke() ?: playlistSelection(library, item)).use { selection ->
                                        model.connectedService().runtime.value!!.getOrThrow().source().use { source ->
                                            addPlaylistTracks(source, (destination as Route.PlaylistDetail).v1, selection, skipDuplicates)
                                        }
                                    }
                                }
                            }
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
    if (create && folderKind != null) PlaylistFolderNameDialog(model, folderKind, {
        create = false
        pages.refresh()
    })
    if (create && folderKind == null) PlaylistCreateSheet(model, { create = false }) { name, sourceId, public ->
        browse.library?.let { library ->
            (selectionOf?.invoke() ?: playlistSelection(library, item)).use { selection ->
                model.connectedService().runtime.value!!.getOrThrow().source().use { source ->
                    createPlaylist(source, sourceId, name, selection, public)?.settingsError?.let { error(it) }
                }
            }
        }
        create = false
        onDismiss()
    }
}

private suspend fun playlistSelection(library: io.github.screwys.rufin.core.AndroidLibrary, item: BrowseItem): MemberSelection =
    if (item.row.isTrack) MemberSelection.fromUris(listOf(item.row.mediaUri))
    else library.routeSelection(item.row.route!!)

@Composable
internal fun PlaylistCreateSheet(
    model: RufinConnection,
    onDismiss: () -> Unit,
    // Name, owning source and public.
    save: suspend (String, String?, Boolean?) -> Unit,
) {
    // A playlist made for a source that cannot own one is a Rufin playlist, with no choice.
    val source = model.sources?.let { state -> state.sources.firstOrNull { it.id == state.selectedSourceId } }
        ?.takeIf { it.ownsPlaylists }
    var name by remember { mutableStateOf("") }
    var currentSource by remember(source?.id) { mutableStateOf(source != null) }
    var public by remember { mutableStateOf(false) }
    val supportsPublic = currentSource && source?.supportsPlaylistPublic == true
    AlertDialog(onDismissRequest = onDismiss,
        title = { Text(translate("New Playlist")) },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedTextField(name, { name = it }, label = { Text(translate("Playlist name")) }, singleLine = true,
                colors = settingsTextFieldColors(),
                modifier = Modifier.fillMaxWidth(), trailingIcon = if (source == null) null else ({
                    IconToggleButton(currentSource, { currentSource = it },
                        modifier = Modifier.semantics {
                            contentDescription = if (currentSource) source.name else "Rufin"
                        }) {
                        if (currentSource) SourceIcon(source.kind)
                        else Image(painterResource(R.drawable.app_icon), null, Modifier.size(24.dp))
                    }
                }))
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

internal class BrowseChoiceSource(
    private val library: AndroidLibrary?, private val route: Route?, private val filter: String,
    private val folderKind: Boolean? = null,
) : PagingSource<Int, BrowseItem>() {
    /** The page it lists, opened once for the filter it reads. */
    private var opened: AndroidOpened? = null
    init { registerInvalidatedCallback { opened?.view?.destroy() } }
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, BrowseItem> = try {
        withContext(Dispatchers.Default) {
            val offset = params.key ?: 0
            fun items(rows: List<AndroidBrowseRow>) = rows.mapIndexed { index, row ->
                BrowseItem(row, (offset + index).toULong(), null)
            }
            if (library == null || route == null) LoadResult.Page(emptyList(), null, null)
            else {
                if (route == Route.Playlists) {
                    val page = library.playlistChoices(filter, folderKind, offset.toULong(), params.loadSize.toUInt())
                    val count = page.count.toInt()
                    val rows = page.rows
                    return@withContext LoadResult.Page(items(rows),
                        if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                        if (offset + rows.size >= count) null else offset + rows.size,
                        itemsBefore = offset, itemsAfter = (count - offset - rows.size).coerceAtLeast(0))
                }
                val page = opened ?: library.open(route, CollectionCategory.TRACKS, filter, LibraryField.TITLE, false, false, false,
                    null, null, true).also { opened = it }
                val count = page.count.toInt()
                val rows = page.view.rows(offset.toULong(), params.loadSize.toUInt(), false).rows
                LoadResult.Page(items(rows), if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                    if (offset + rows.size >= count) null else offset + rows.size,
                    itemsBefore = offset, itemsAfter = (count - offset - rows.size).coerceAtLeast(0))
            }
        }
    } catch (cancelled: CancellationException) { throw cancelled }
    catch (error: Exception) { LoadResult.Error(error) }
    override fun getRefreshKey(state: PagingState<Int, BrowseItem>): Int? = state.anchorPosition
}
