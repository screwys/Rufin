package io.github.screwys.rufin.app

import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.browse.*
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract

import io.github.screwys.rufin.R
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.browse.AppSection
import io.github.screwys.rufin.browse.BrowseOptions
import io.github.screwys.rufin.browse.BrowseLocation
import io.github.screwys.rufin.browse.BrowseItem
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.browse.BrowseContent
import io.github.screwys.rufin.browse.BrowseSortSheet
import io.github.screwys.rufin.browse.BrowseMenuSheet
import io.github.screwys.rufin.browse.PlaylistPickerSheet
import io.github.screwys.rufin.browse.PlaylistCreateSheet
import io.github.screwys.rufin.metadata.MetadataEditorScreen
import io.github.screwys.rufin.connect.ConnectPairingPrompt
import io.github.screwys.rufin.connect.ConnectProgressScreen
import io.github.screwys.rufin.browse.BrowsePage
import io.github.screwys.rufin.browse.DetailContent
import io.github.screwys.rufin.browse.DetailTrackRow
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.player.MiniPlayer
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.more.MoreContent
import io.github.screwys.rufin.browse.PinsContent
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.rememberPlayerConnection
import io.github.screwys.rufin.player.PlayerScreen
import io.github.screwys.rufin.player.PlayerOutputSheet
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.sources.SourceSetupScreen
import io.github.screwys.rufin.sources.SourceSelector
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.browse.RandomPlaySheet
import io.github.screwys.rufin.browse.BrowseGestureSettings
import io.github.screwys.rufin.browse.SearchContent
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding


import android.os.Build
import android.app.Activity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.SeekableTransitionState
import androidx.compose.animation.core.rememberTransition
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.material3.ripple
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshDefaults
import androidx.compose.material3.pulltorefresh.pullToRefresh
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.clip
import androidx.compose.ui.zIndex
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.animation.animateColorAsState
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalContext
import androidx.media3.common.util.UnstableApi
import androidx.paging.LoadState
import androidx.paging.compose.LazyPagingItems
import androidx.paging.compose.itemKey
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.PagingData
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import io.github.screwys.rufin.ui.pageMotion

private data class BrowseDestination(val root: String, val depth: Int, val location: BrowseLocation?)

@Composable
internal fun FeedbackToast(model: RufinConnection, player: PlayerConnection) {
    val feedback = model.feedback ?: return
    androidx.compose.ui.window.Popup(alignment = Alignment.BottomCenter, properties = androidx.compose.ui.window.PopupProperties(focusable = false)) {
        Surface(Modifier.navigationBarsPadding().padding(horizontal = 16.dp, vertical = 20.dp).widthIn(max = 360.dp),
            shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh, shadowElevation = 4.dp) {
            Row(Modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                feedback.artworkIdentity?.let { identity ->
                    val pixels = with(androidx.compose.ui.platform.LocalDensity.current) { 40.dp.roundToPx() }
                    io.github.screwys.rufin.ui.Artwork(io.github.screwys.rufin.ui.rememberArtwork(identity, player, pixels),
                        Modifier.size(40.dp).clip(RoundedCornerShape(6.dp)))
                }
                Text(feedback.message, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}

@androidx.annotation.OptIn(UnstableApi::class)
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class, ExperimentalLayoutApi::class, androidx.compose.animation.ExperimentalAnimationApi::class)
@Composable
internal fun LibraryScreen(model: RufinConnection, preferences: PreferencesConnection,
    onOpen: () -> Unit, onAddFolder: () -> Unit, onSaveLog: () -> Unit) {
    val translation = LocalTranslationRevision.current
    val reduceMotion = LocalReduceMotion.current
    val label: (String) -> String = ::translate
    val resetScope = rememberCoroutineScope()
    var confirmDatabaseReset by remember { mutableStateOf(false) }
    var resettingDatabase by remember { mutableStateOf(false) }
    val browse = model.browse
    SideEffect {
        browse.homeBlocksFingerprint = preferences.applicationSettings?.homeBlocks?.toString()
        browse.showDownloadedBadges = preferences.applicationSettings?.showDownloadedBadges != false
    }
    val player = rememberPlayerConnection(model)
    FeedbackToast(model, player)
    val connectState = model.connect.state
    if (model.connect.operationActive && connectState != null && connectState.pairing == null && (connectState.connecting || model.connect.visibleError != null)) {
        BackHandler { model.connect.dismissOperation() }
        ConnectProgressScreen(model)
        return
    }
    SideEffect { model.controlNotifications = preferences.applicationSettings?.controlNotificationsEnabled != false }
    var sources by rememberSaveable { mutableStateOf(false) }
    var sourceMenu by remember { mutableStateOf(false) }
    var editSourceId by rememberSaveable { mutableStateOf<String?>(null) }
    var fullPlayer by rememberSaveable { mutableStateOf(false) }
    var outputs by rememberSaveable { mutableStateOf(false) }
    var searchExpanded by rememberSaveable { mutableStateOf(false) }
    val searchFocus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    val focusManager = LocalFocusManager.current
    var focusSearch by remember { mutableIntStateOf(0) }
    LaunchedEffect(focusSearch) { if (focusSearch > 0) { searchFocus.requestFocus(); keyboard?.show() } }
    var sort by rememberSaveable { mutableStateOf(false) }
    var sortPage by remember { mutableStateOf<BrowsePage?>(null) }
    var random by rememberSaveable { mutableStateOf(false) }
    var menu by remember { mutableStateOf<BrowseItem?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var createPlaylist by rememberSaveable { mutableStateOf(false) }
    var createMenu by remember { mutableStateOf(false) }
    var createFolder by rememberSaveable { mutableStateOf(false) }
    var createSmartPlaylist by rememberSaveable { mutableStateOf(false) }
    val playlistFolder = browse.current?.route as? Route.PlaylistFolder
    val playlistList = browse.current?.route.let { it is Route.Playlists || it is Route.SmartPlaylists || it is Route.PlaylistFolder }
    val smartPlaylistList = playlistFolder?.smart ?: (browse.current?.route is Route.SmartPlaylists)
    val context = LocalContext.current
    val importTheme = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(preferences::importTheme) }
    val importPlaylist = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == Activity.RESULT_OK) {
            val intent = result.data
            val uris = intent?.clipData?.let { clip -> (0 until clip.itemCount).map { clip.getItemAt(it).uri } }
                ?: listOfNotNull(intent?.data)
            model.runAction {
                uris.forEach { uri ->
                    if (intent != null && intent.flags and Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION != 0 &&
                        intent.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
                        context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                    val imported = browse.library?.importPlaylist(uri.toString()) ?: return@forEach
                    playlistFolder?.let { folder ->
                        imported.row.playlistFolderMember?.let { member ->
                            model.sourceOwner?.let { movePlaylistToFolder(it, member, folder.key) }
                        }
                    }
                    browse.open(BrowseItem(imported.row, 0UL, null))
                    if (imported.skipped > 0UL) model.showFeedback(translate("Some playlist entries could not be imported"))
                }
                browse.refresh()
            }
        }
    }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    var scrolled by remember { mutableStateOf(false) }
    var moreStack by rememberSaveable { mutableStateOf(listOf(MoreDestination.Root)) }
    val moreDestination = moreStack.last()
    fun openMore(destination: MoreDestination) {
        moreStack = generateSequence(destination) { it.parent }.toList().reversed()
        browse.section = AppSection.More
    }
    LaunchedEffect(model.more.startupPeriod) {
        if (model.more.startupPeriod != null) {
            model.more.openStartupOverview()
            openMore(MoreDestination.Overview)
        }
    }
    var routeMenu by remember { mutableStateOf(false) }
    val routePosition = animateFloatAsState(
        browse.configuredRoutes.indexOfFirst { it.id == browse.rootId }.coerceAtLeast(0).toFloat(),
        tween(if (reduceMotion) 0 else 180), label = "libraryTab")
    val routeState = rememberSaveableStateHolder()
    val detailState = rememberSaveableStateHolder()
    val rootPages = browse.routes.filter { route ->
        route.id != "Search" && (route.id == browse.rootId || browse.configuredRoutes.any { it.id == route.id })
    }.associate { route -> key(route.id) {
        val options = remember(route.id) { browse.optionsFor(route.id) }
        val location = remember(route.id, route.route) { BrowseLocation(route.route, route.title, route.id, options) }
        val lifetime = rememberCoroutineScope()
        val page = remember(route.route, options) { BrowsePage(model, browse, location, options, lifetime) }
        LaunchedEffect(page) { page.observe() }
        route.id to page
    } }
    val detailPages = browse.details.associateWith { location -> key(location) {
        val lifetime = rememberCoroutineScope()
        val page = remember(location) { BrowsePage(model, browse, location, location.options, lifetime) }
        LaunchedEffect(page) { page.observe() }
        page
    } }
    val pinsLifetime = rememberCoroutineScope()
    val pinsPage = remember(model, browse) {
        val options = BrowseOptions()
        val location = BrowseLocation(Route.Home, "Pins", "Pins", options)
        BrowsePage(model, browse, location, options, pinsLifetime, pins = true)
    }
    LaunchedEffect(pinsPage) { pinsPage.observe() }
    val destination = BrowseDestination(browse.rootId, browse.details.size, browse.details.lastOrNull())
    val destinationPage = if (destination.location != null) detailPages[destination.location]
        else rootPages[destination.root]?.takeIf {
            browse.library != null && browse.routes.any { it.id == destination.root } &&
                (model.sources?.sources?.isEmpty() != true || destination.root in listOf("History", "Playlists", "SmartPlaylists"))
        }
    val navigation = remember(browse.section) { SeekableTransitionState(destination) }
    val navigationTransition = rememberTransition(navigation, label = "libraryNavigation")
    LaunchedEffect(destination, destinationPage, browse.section, reduceMotion) {
        if (browse.section != AppSection.Browse) return@LaunchedEffect
        navigation.seekTo(0f, destination)
        destinationPage?.let { page -> snapshotFlow { !page.loading }.first { it } }
        if (reduceMotion) navigation.snapTo(destination) else navigation.animateTo(destination)
    }
    val hasMedia by remember(player) { derivedStateOf { player.currentId?.mediaUri != null } }
    val density = LocalDensity.current
    val playerPadding = if (hasMedia) maxOf(52.dp, with(density) {
        MaterialTheme.typography.bodyMedium.lineHeight.toDp() + MaterialTheme.typography.bodySmall.lineHeight.toDp()
    } + 3.dp) + 10.dp else 0.dp
    LaunchedEffect(translation) { browse.refreshLabels(translation) }
    fun selectRoute(id: String) {
        routeMenu = false
        keyboard?.hide(); focusManager.clearFocus(); searchExpanded = false
        if (id == "Search") { browse.searchText = ""; browse.section = AppSection.Search }
        else { browse.chooseRoot(id); browse.section = AppSection.Browse }
    }
    BackHandler(searchExpanded || browse.detail || browse.section != AppSection.Browse) {
        when {
            browse.section == AppSection.More && moreStack.size > 1 -> moreStack = moreStack.dropLast(1)
            searchExpanded -> searchExpanded = false
            browse.section != AppSection.Browse -> browse.section = AppSection.Browse
            else -> browse.back()
        }
    }
    BrowseGestureSettings(preferences.swipeLeft, preferences.swipeRight, browse, { playlist = it }) {
        Scaffold(topBar = {
            TopAppBar(expandedHeight = 48.dp, navigationIcon = {
                if (browse.section == AppSection.Browse && browse.detail) PlayerIconButton("rufin-go-previous-symbolic", "Back", browse::back)
                else if (browse.section == AppSection.More && moreDestination != MoreDestination.Root)
                    PlayerIconButton("rufin-go-previous-symbolic", "Back", { moreStack = moreStack.dropLast(1) })
                else Box(Modifier.padding(start = 4.dp)) {
                    Image(painterResource(R.drawable.app_icon), label("Select Source"),
                        Modifier.size(48.dp).combinedClickable(onClick = { sourceMenu = true },
                            onLongClick = { sourceMenu = true }, onLongClickLabel = label("Select Source")).padding(10.dp))
                    SourceSelector(sourceMenu, model, browse, { sourceMenu = false },
                        manage = { openMore(MoreDestination.Library) },
                        add = { editSourceId = null; sources = true })
                }
            }, title = {
                if (searchExpanded || browse.section == AppSection.Search) BasicTextField(
                    modifier = Modifier.fillMaxWidth().focusRequester(searchFocus).onFocusChanged { if (it.isFocused) searchExpanded = true },
                    value = if (browse.section == AppSection.Search) browse.searchText else browse.filter,
                    onValueChange = { if (browse.section == AppSection.Search) browse.searchText = it else browse.filter = it },
                    singleLine = true, textStyle = MaterialTheme.typography.titleMedium.copy(color = MaterialTheme.colorScheme.onSurface),
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary), keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    keyboardActions = KeyboardActions(onSearch = { focusManager.clearFocus(); keyboard?.hide(); searchExpanded = false }),
                    decorationBox = { input -> Box(Modifier.padding(horizontal = 8.dp, vertical = 8.dp)) {
                        if ((if (browse.section == AppSection.Search) browse.searchText else browse.filter).isBlank())
                            Text(label("Search"), style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        input()
                    } })
                else Text(when (browse.section) {
                    AppSection.Browse -> browse.current?.title ?: label("Library")
                    AppSection.Search -> label("Search")
                    AppSection.Pins -> label("Pins")
                    AppSection.More -> label(moreDestination.title)
                }, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleLarge)
            }, actions = {
                if (browse.section == AppSection.More && moreDestination == MoreDestination.Theme) {
                    IconButton({ importTheme.launch(arrayOf("*/*")) }, enabled = preferences.state != null) {
                        RufinIcon("rufin-document-open-symbolic", translate("Import theme"))
                    }
                }
                if (browse.section == AppSection.Browse && playlistList) {
                    Box {
                        PlayerIconButton("rufin-list-add-symbolic", "Create", { createMenu = true })
                        DropdownMenu(createMenu, { createMenu = false }) {
                            DropdownMenuItem(text = { Text(translate(if (smartPlaylistList) "New Smart Playlist" else "New Playlist")) }, onClick = {
                                createMenu = false
                                if (smartPlaylistList) createSmartPlaylist = true else createPlaylist = true
                            })
                            if (playlistFolder == null) DropdownMenuItem(text = { Text(translate("New folder")) }, onClick = { createMenu = false; createFolder = true })
                        }
                    }
                    if (!smartPlaylistList) PlayerIconButton("rufin-document-open-symbolic", "Import Playlist", {
                        importPlaylist.launch(Intent(Intent.ACTION_OPEN_DOCUMENT).setType("*/*")
                            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION))
                    })
                }
                if (browse.section != AppSection.More) PlayerIconButton(if (searchExpanded) "rufin-window-close-symbolic" else "rufin-search-symbolic",
                    if (searchExpanded) "Close" else "Search", {
                        when {
                            searchExpanded -> { searchExpanded = false; focusManager.clearFocus(); keyboard?.hide() }
                            browse.section == AppSection.Pins -> { browse.section = AppSection.Search; searchExpanded = true; focusSearch++ }
                            else -> { searchExpanded = true; focusSearch++ }
                        }
                    })
                if (browse.section == AppSection.Browse && (browse.detail || browse.rootId != "Home"))
                    PlayerIconButton("rufin-filter-symbolic", "Customize", { sortPage = browse.visiblePage; sort = true })
            })
        }, bottomBar = {
            Column {
                Row(Modifier.fillMaxWidth().navigationBarsPadding(), horizontalArrangement = Arrangement.SpaceEvenly) {
                    listOf(Triple(AppSection.Browse, label("Library"), "rufin-library-symbolic"),
                        Triple(AppSection.Search, label("Search"), "rufin-search-symbolic"),
                        Triple(AppSection.Pins, label("Pins"), "rufin-bookmark-symbolic"),
                        Triple(AppSection.More, label("More"), "rufin-dots-horizontal-symbolic")).forEach { (section, title, icon) ->
                        val selected = browse.section == section && !(section == AppSection.Browse && browse.detail)
                        val tint by animateColorAsState(if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                            animationSpec = tween(if (reduceMotion) 0 else 100), label = "navigationTint")
                        Box(Modifier.weight(1f)) {
                        Column(Modifier.fillMaxWidth().combinedClickable(role = Role.Tab,
                            interactionSource = remember { MutableInteractionSource() }, indication = ripple(bounded = false, radius = 28.dp),
                            onLongClick = if (section == AppSection.Browse) ({ routeMenu = true }) else null,
                            onLongClickLabel = if (section == AppSection.Browse) label("Library") else null, onClick = {
                            when {
                                section == AppSection.Browse && browse.detail -> {
                                    keyboard?.hide(); focusManager.clearFocus(); routeMenu = false; searchExpanded = false
                                    browse.chooseRoot("Home"); browse.section = AppSection.Browse
                                }
                                section == AppSection.Browse && selected -> {
                                    keyboard?.hide(); focusManager.clearFocus(); searchExpanded = false
                                    routeMenu = true
                                }
                                section == AppSection.Search && selected -> focusSearch++
                                section == AppSection.More && selected -> openMore(MoreDestination.Root)
                                else -> {
                                    keyboard?.hide(); focusManager.clearFocus(); routeMenu = false
                                    if (section == AppSection.Search) browse.searchText = ""
                                    if (section == AppSection.More) moreStack = listOf(MoreDestination.Root)
                                    if (section == AppSection.Pins && !selected) pinsPage.refresh()
                                    browse.section = section; searchExpanded = false
                                }
                            }
                        }).semantics { this.selected = selected }.heightIn(min = 56.dp).padding(vertical = 6.dp), horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(3.dp, Alignment.CenterVertically)) {
                            RufinIcon(icon, null, Modifier.size(24.dp), tint)
                            Text(title, color = tint, style = MaterialTheme.typography.labelSmall)
                        }
                        if (section == AppSection.Browse) DropdownMenu(routeMenu, { routeMenu = false }, Modifier.widthIn(min = 180.dp, max = 280.dp)) {
                            browse.configuredRoutes.forEach { route -> DropdownMenuItem(
                                text = { Text(route.title) }, onClick = { selectRoute(route.id) },
                                leadingIcon = { RufinIcon(if (route.id == browse.rootId) route.selectedIconName else route.iconName, null,
                                    tint = if (route.id == browse.rootId) MaterialTheme.colorScheme.primary else LocalContentColor.current) }) }
                        }
                        }
                    }
                }
            }
        }, floatingActionButton = {
            if (browse.section == AppSection.Browse && browse.rootId == "Home" && !browse.detail && !scrolled)
                FloatingActionButton({ random = true }, Modifier.padding(bottom = playerPadding)) { RufinIcon("rufin-random-symbolic", label("Play random")) }
        }) { padding ->
            Box(Modifier.fillMaxSize().padding(padding)) {
            CompositionLocalProvider(LocalPlayerBottomPadding provides playerPadding) {
            Column(Modifier.fillMaxSize()) {
                if (!model.connect.operationActive && connectState?.let { it.connecting } == true) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(label("Rufin Connect"), Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                        TextButton(model.connect::showOperation) { Text(label("Open Rufin Connect")) }
                    }
                }
                model.startupError?.let { failure ->
                    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
                        ErrorNotice(failure)
                        FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                            TextButton(model::retryStartup, enabled = !resettingDatabase) { Text(label("Retry")) }
                            TextButton({ confirmDatabaseReset = true }, enabled = !resettingDatabase) { Text(label("Reset database")) }
                        }
                    }
                }
                model.sourceStatus?.let {
                    val download = model.sourceOperation as? SourceOperation.Downloading
                    val progress = model.sourceProgress
                    val total = progress?.total
                    if (progress != null && total != null && total > 0UL) {
                        LinearProgressIndicator(progress = {
                            (progress.completed.toFloat() / total.toFloat()).coerceIn(0f, 1f)
                        }, modifier = Modifier.fillMaxWidth())
                    } else LinearProgressIndicator(Modifier.fillMaxWidth())
                    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(it, Modifier.weight(1f).padding(vertical = 8.dp), style = MaterialTheme.typography.bodySmall)
                        if (download != null) TextButton({ model.runAction { model.sourceOwner?.cancelSourceDownload(download.sourceId) } }) {
                            Text(label("Cancel"))
                        }
                    }
                }
                val refreshState = rememberPullToRefreshState()
                val refreshing = (model.sourceOperation as? SourceOperation.Refreshing)?.let {
                    it.sourceId == model.sources?.selectedSourceId
                } == true
                val canRefresh = model.ready && model.sources?.selectedSourceId != null &&
                    browse.section != AppSection.More && !fullPlayer
                Box(Modifier.weight(1f).pullToRefresh(isRefreshing = refreshing, state = refreshState,
                    enabled = canRefresh && !model.sourceBusy, onRefresh = model::refresh)) {
                    if (browse.section == AppSection.Browse) navigationTransition.AnimatedContent(
                        Modifier.fillMaxSize(), transitionSpec = {
                            if (initialState.depth == 0 && targetState.depth == 0)
                                fadeIn(tween(if (reduceMotion) 0 else 180)) togetherWith fadeOut(tween(if (reduceMotion) 0 else 180))
                            else pageMotion(targetState.depth > initialState.depth, reduceMotion)
                        }, contentKey = { it.location ?: it.root },
                    ) { visible ->
                        val location = visible.location
                        if (location == null) Column(Modifier.fillMaxSize()) {
                            if (browse.configuredRoutes.isNotEmpty()) BrowseRouteTabs(browse, routePosition) { id -> selectRoute(id) }
                            val routeId = visible.root
                            Box(Modifier.weight(1f).fillMaxWidth()) {
                                if (browse.library == null) {
                                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                                        CircularProgressIndicator(Modifier.semantics { contentDescription = label("Loading...") })
                                    }
                                } else if (model.sources?.sources?.isEmpty() == true && routeId !in listOf("History", "Playlists", "SmartPlaylists")) {
                                    Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
                                        RufinIcon("rufin-library-symbolic", null, Modifier.size(48.dp))
                                        Text(translate("No sources configured"), Modifier.padding(16.dp), style = MaterialTheme.typography.titleMedium)
                                        Button({ editSourceId = null; sources = true }) { Text(translate("Add Source")) }
                                    }
                                } else
                                browse.routes.firstOrNull { it.id == routeId }?.let { route ->
                                val options = remember(route.id) { browse.optionsFor(route.id) }
                                val location = remember(route.id, route.route) { BrowseLocation(route.route, route.title, route.id, options) }
                                routeState.SaveableStateProvider(route.id) {
                                    BrowsePageContent(model, browse, player, location, options, !fullPlayer && !browse.detail && route.id == browse.rootId,
                                        { menu = it }, { playlist = it }, { scrolled = it }, owner = rootPages[route.id])
                                }
                                } ?: if (browse.configuredRoutes.none { it.id != "Search" }) Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally,
                                    verticalArrangement = Arrangement.Center) {
                                    RufinIcon("rufin-library-symbolic", null, Modifier.size(48.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                                    TextButton({ openMore(MoreDestination.Routes) }) { Text(label("Library")) }
                                } else Spacer(Modifier.fillMaxSize())
                            }
                        } else detailState.SaveableStateProvider("${visible.depth}:${location.route}") {
                            val page = remember(location) { detailPages[location] }
                            DisposableEffect(location) {
                                onDispose { if (location !in browse.details) detailState.removeState("${visible.depth}:${location.route}") }
                            }
                            Surface(Modifier.fillMaxSize()) {
                                BrowsePageContent(model, browse, player, location, location.options,
                                    !fullPlayer && browse.details.lastOrNull() === location,
                                    { menu = it }, { playlist = it }, { scrolled = it }, owner = page)
                            }
                        }
                    }
                if (browse.section != AppSection.Browse) Box(Modifier.fillMaxSize()) {
                    if (browse.section == AppSection.More) {
                        routeState.SaveableStateProvider("section:More") {
                        MoreContent(model, preferences, player, browse, moreDestination, { moreStack = moreStack + it },
                            onAddSource = { editSourceId = null; sources = true }, onEditSource = { editSourceId = it; sources = true },
                            onOpen = onOpen, onSaveLog = onSaveLog, onOutput = { outputs = true }, navigationDepth = moreStack.size)
                        }
                    } else if (browse.section == AppSection.Search) {
                        SearchContent(browse, player, { menu = it }, { playlist = it }, { scrolled = it })
                    } else if (browse.section == AppSection.Pins) {
                        routeState.SaveableStateProvider("section:Pins") { PinsContent(model, browse, player, pinsPage) }
                    }
                }
                    if (canRefresh) PullToRefreshDefaults.Indicator(state = refreshState, isRefreshing = refreshing,
                        modifier = Modifier.align(Alignment.TopCenter))
                }
            }
            }
            if (hasMedia) Box(Modifier.align(Alignment.BottomCenter)) { MiniPlayer(player, { fullPlayer = true }, { outputs = true }) }
            }
        }
    }
    AnimatedVisibility(fullPlayer, enter = slideInVertically(tween(if (reduceMotion) 0 else 220)) { it }, exit = slideOutVertically(tween(if (reduceMotion) 0 else 180)) { it }) {
        PlayerScreen(player, preferences, onNavigate = { route, title, source ->
            fullPlayer = false
            browse.openLinkedRoute(route, title, source)
        }, onMore = {
            player.currentItem?.let { song -> browse.library?.trackItem(song)?.let { menu = BrowseItem(it, 0UL, null) } }
        }) { fullPlayer = false }
    }
    if (confirmDatabaseReset) AlertDialog(onDismissRequest = { confirmDatabaseReset = false },
        title = { Text(label("Reset database?")) },
        text = { Text(label("Remove the library database? Settings and saved logins will be kept.")) },
        confirmButton = { TextButton({
            confirmDatabaseReset = false
            resettingDatabase = true
            resetScope.launch {
                try { model.resetDatabase() }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: Exception) { model.showFeedback(failure); resettingDatabase = false }
            }
        }) { Text(label("Reset database")) } },
        dismissButton = { TextButton({ confirmDatabaseReset = false }) { Text(label("Cancel")) } })
    if (resettingDatabase) Dialog(onDismissRequest = {}, properties = DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false)) {
        Surface(shape = MaterialTheme.shapes.large) { CircularProgressIndicator(Modifier.padding(32.dp)) }
    }
    if (outputs) PlayerOutputSheet(player) { outputs = false }
    if (sort) BrowseSortSheet(browse, { sort = false }, sortPage)
    if (random) RandomPlaySheet(model, browse) { random = false }
    menu?.let { item -> BrowseMenuSheet(model, browse, item, player, { menu = null }, { playlist = item; menu = null }, { metadata = item; menu = null },
        onNavigate = { route, title, source -> menu = null; fullPlayer = false; browse.openLinkedRoute(route, title, source) }) }
    playlist?.let { item -> PlaylistPickerSheet(model, browse, item, player) { playlist = null } }
    if (createPlaylist) PlaylistCreateSheet(model, { createPlaylist = false },
        initialCurrentSource = preferences.applicationSettings?.newPlaylistCurrent != false) { name, sourceId, public ->
        io.github.screwys.rufin.shared.library.MemberSelection.fromUris(emptyList()).use { selection ->
            val source = model.sourceOwner ?: return@PlaylistCreateSheet
            io.github.screwys.rufin.shared.rufin_core.createPlaylist(source, sourceId, name, selection, public, null)?.let { created ->
                playlistFolder?.let { folder ->
                    created.playlistKey?.let { key -> movePlaylistToFolder(source,
                        io.github.screwys.rufin.shared.library.PlaylistFolderMember.Playlist(key), folder.key) }
                }
                created.settingsError?.let { error(it) }
            }
        }
        browse.refresh()
        createPlaylist = false
    }
    if (createFolder) PlaylistFolderNameDialog(model, browse, smartPlaylistList, { createFolder = false })
    if (createSmartPlaylist) SmartPlaylistEditor(model, browse, { createSmartPlaylist = false }, folder = playlistFolder?.key)
    metadata?.let { item -> MetadataEditorScreen(model, item.row.kind, item.row.mediaUri) { metadata = null } }
    if (metadata == null && menu == null && playlist == null && !createPlaylist && !createFolder && !createSmartPlaylist && !sources && !outputs && !sort && !random) {
        ConnectPairingPrompt(model) { openMore(MoreDestination.Connect) }
    }
    if (sources) Dialog(onDismissRequest = { model.closeSourceSetup(); sources = false },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        SourceSetupScreen(model, { model.closeSourceSetup(); sources = false }, editSourceId)
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun BrowseRouteTabs(browse: BrowseConnection, position: State<Float>, onSelect: (String) -> Unit) {
    val selected = browse.configuredRoutes.indexOfFirst { it.id == browse.rootId }
    PrimaryScrollableTabRow(selected.coerceAtLeast(0), Modifier.fillMaxWidth().height(40.dp),
        edgePadding = 8.dp, minTabWidth = 0.dp, containerColor = Color.Transparent, divider = {}, indicator = {
            if (selected >= 0) TabRowDefaults.PrimaryIndicator(
                Modifier.zIndex(-1f).tabIndicatorLayout { measurable, constraints, tabs ->
                    if (tabs.isEmpty()) return@tabIndicatorLayout layout(0, 0) {}
                    val value = position.value.coerceIn(0f, tabs.lastIndex.toFloat())
                    val from = value.toInt()
                    val to = (from + 1).coerceAtMost(tabs.lastIndex)
                    val fraction = value - from
                    val left = (tabs[from].left.value + (tabs[to].left.value - tabs[from].left.value) * fraction).dp.roundToPx()
                    val width = (tabs[from].width.value + (tabs[to].width.value - tabs[from].width.value) * fraction).dp.roundToPx()
                    val centeringOffset = ((tabs[selected].width.roundToPx() - constraints.maxWidth) / 2).coerceAtLeast(0)
                    val height = 32.dp.roundToPx()
                    val placed = measurable.measure(constraints.copy(minWidth = width, maxWidth = width, minHeight = height, maxHeight = height))
                    // The tab row already centers the indicator wrapper inside the selected tab.
                    layout(constraints.maxWidth, constraints.maxHeight) { placed.placeRelative(left - centeringOffset, (constraints.maxHeight - height) / 2) }
                }, height = 32.dp, width = androidx.compose.ui.unit.Dp.Unspecified,
                color = MaterialTheme.colorScheme.secondaryContainer, shape = RoundedCornerShape(8.dp))
        }) {
        browse.configuredRoutes.forEachIndexed { index, route ->
            val active = index == selected
            val tint = lerp(MaterialTheme.colorScheme.onSurfaceVariant, MaterialTheme.colorScheme.onSecondaryContainer,
                (1f - kotlin.math.abs(position.value - index)).coerceIn(0f, 1f))
            Tab(active, { onSelect(route.id) }, Modifier.height(40.dp),
                selectedContentColor = tint, unselectedContentColor = tint,
                text = { Text(route.title, maxLines = 1, color = tint, style = MaterialTheme.typography.bodyMedium) })
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun BrowsePageContent(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection,
    location: BrowseLocation, options: BrowseOptions, active: Boolean,
    onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit, onScrolled: (Boolean) -> Unit, pins: Boolean = false,
    owner: BrowsePage? = null,
) {
    val scope = rememberCoroutineScope()
    LaunchedEffect(location.kind, model.sources?.selectedSourceId) {
        if (location.kind == "History" && model.sources?.selectedSourceId == null && options.category == "current") options.category = "all"
    }
    val page = owner ?: remember(location.route, options, pins) { BrowsePage(model, browse, location, options, scope, pins) }
    if (owner == null) LaunchedEffect(page) { page.observe() }
    DisposableEffect(page) { onDispose { page.active = false } }
    SideEffect { page.active = active; if (active) browse.visiblePage = page }
    val rows = page.pages.collectAsLazyPagingItems()
    if (page.summary?.showsReleases != true && page.homeRows.isEmpty() && rows.itemSnapshotList.items.isEmpty() &&
        (page.loading || rows.loadState.refresh is LoadState.Loading)) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val summary = page.summary
    val choices = when (location.kind) {
        "Favorites" -> browse.categories
        "artist" -> browse.categories.filter { it.id != "artists" }
        else -> emptyList()
    }
    Column(Modifier.fillMaxSize()) {
        if (location.kind == "History") {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(options.category != "current", { options.category = "all" }, label = { Text(translate("All")) })
                if (model.sources?.selectedSourceId != null) FilterChip(options.category == "current", { options.category = "current" },
                    label = { Text(model.selectedSourceName) })
            }
            HistoryRows(rows, page, player, browse, onMenu, onPlaylist, onScrolled)
            return@Column
        }
        if (location.kind in listOf("Folders", "folder")) FolderNavigation(model, browse, page)
        if (summary != null) {
            val context = androidx.compose.ui.platform.LocalContext.current
            DetailContent(summary, page, player, page.favoritesPage, page.releasePages,
                browse::open, onMenu, { item, placement, shuffled -> browse.play(item, placement, shuffled) }, browse::favorite,
                onPlaylist, { route, title -> browse.openRoute(route, title, route.kind, summary.row.title) },
                onLink = { link -> model.runAction {
                    var uri = Uri.parse(link.url ?: page.sourceUri() ?: return@runAction)
                    val intent = Intent(Intent.ACTION_VIEW)
                    if (uri.scheme == "content") {
                        if (DocumentsContract.isTreeUri(uri) && !DocumentsContract.isDocumentUri(context, uri)) {
                            uri = DocumentsContract.buildDocumentUriUsingTree(uri, DocumentsContract.getTreeDocumentId(uri))
                        }
                        intent.setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)
                    } else intent.data = uri
                    context.startActivity(intent)
                } }, onScrolled, onRadio = { browse.radio(it, "now") })
            return@Column
        }
        if (choices.isNotEmpty()) LazyRow(Modifier.fillMaxWidth(), contentPadding = PaddingValues(horizontal = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            itemsIndexed(choices, key = { _, option -> option.id }) { _, option ->
                FilterChip(options.category == option.id, { options.category = option.id; options.sort = null; options.displaySettings = null },
                    label = { Text(option.title) })
            }
        }
        Box(Modifier.weight(1f)) {
            val display = options.displaySettings ?: page.displaySettings ?: rows.itemSnapshotList.items.firstOrNull()?.displaySettings
            val covers = if (display != null) display.layout == LibraryLayout.GRID else page.covers
            BrowseContent(rows, page.homeRows, player, location.kind == "Home", location in browse.details, covers,
                browse::open, onMenu, { id, shuffled -> page.playSection(id, shuffled) }, browse::refreshSection,
                browse::favorite, onPlaylist, { if (active) onScrolled(it) }, page.scrollSections,
                display?.size ?: DisplaySize.DEFAULT, display?.gridSpacing ?: DisplaySize.DEFAULT, { item, shuffled -> browse.play(item, shuffled = shuffled) }, page.detailTrackFields,
                page.supportsLetterIndex, page::letterPosition, page.sortSelection?.descending ?: false,
                page.hasActiveFilters, page::resetFilters, page.loading, onRadio = { browse.radio(it, "now") },
                emptyIcon = if (location.kind in listOf("Folders", "folder")) "rufin-folders-symbolic" else null)
        }
    }
}

@Composable
private fun FolderNavigation(model: RufinConnection, browse: BrowseConnection, page: BrowsePage) {
    var chooser by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        LazyRow(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            itemsIndexed(page.breadcrumbs, key = { _, crumb -> crumb.route.toString() }) { index, crumb ->
                if (index > 0) Text("/", color = MaterialTheme.colorScheme.onSurfaceVariant)
                TextButton({ browse.folderAncestor(crumb.route, crumb.title) }) { Text(crumb.title, maxLines = 1) }
            }
        }
        page.musicFolders?.let { folders ->
            if (folders.choices.size > 1) Box {
                TextButton({ chooser = true }) {
                    RufinIcon("rufin-folders-symbolic", null, Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text(folders.choices.firstOrNull { it.id == folders.selectedId }?.title.orEmpty(),
                        maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 140.dp))
                }
                DropdownMenu(chooser, { chooser = false }) {
                    folders.choices.forEach { folder -> DropdownMenuItem(text = { Text(folder.title) }, onClick = {
                        chooser = false
                        model.runAction { browse.library?.setMusicFolder(folder.id) }
                    }) }
                }
            }
        }
    }
}

@Composable
private fun HistoryRows(rows: LazyPagingItems<BrowseItem>, page: BrowsePage, player: PlayerConnection,
    browse: BrowseConnection, onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    val state = rememberLazyListState()
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }.collect(onScrolled)
    }
    val fields = page.detailTrackFields
    val images = fields.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
        items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
            val item = rows[index]
            if (item == null) Spacer(Modifier.height(64.dp))
            else DetailTrackRow(item.copy(displaySettings = page.options.displaySettings ?: page.displaySettings),
                player, images, fields, browse::open, onMenu, browse::favorite, onPlaylist)
        }
        if (rows.itemCount == 0 && rows.loadState.refresh is LoadState.NotLoading) item {
            Text(translate("Nothing played yet"), Modifier.padding(24.dp))
        }
        if (rows.loadState.refresh is LoadState.Loading || rows.loadState.append is LoadState.Loading) item {
            LinearProgressIndicator(Modifier.fillMaxWidth())
        }
        val failure = (rows.loadState.refresh as? LoadState.Error)?.error ?: (rows.loadState.append as? LoadState.Error)?.error
        failure?.let { error -> item { ErrorRow(error, translate("Retry"), rows::retry) } }
    }
}
