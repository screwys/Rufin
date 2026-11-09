package io.github.screwys.rufin.app

import io.github.screwys.rufin.ui.ChoiceChip

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.browse.*
import android.content.Intent
import android.net.Uri
import io.github.screwys.rufin.ui.openMediaUri

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
import io.github.screwys.rufin.browse.FieldRow
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.player.PlayerMenuActions
import io.github.screwys.rufin.player.SleepTimerSheet
import io.github.screwys.rufin.player.MiniPlayer
import io.github.screwys.rufin.player.SidebarPlayer
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.more.MoreContent
import io.github.screwys.rufin.browse.PinsContent
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.rememberPlayerConnection
import io.github.screwys.rufin.player.PlayerScreen
import io.github.screwys.rufin.player.PlayerOutputSheet
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.sources.SourceSetupScreen
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
import io.github.screwys.rufin.ui.CoverShape
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.core.tween
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
import androidx.compose.ui.graphics.graphicsLayer
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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import io.github.screwys.rufin.ui.pageMotion
import io.github.screwys.rufin.core.AndroidRouteDescriptor
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalConfiguration

private data class BrowseDestination(val root: String, val depth: Int, val location: BrowseLocation?)

/** The bottom tabs; Library shows the configured routes, Saved the pins. */
private enum class ShellTab(val title: String, val icon: String) {
    Home("Home", "rufin-home-symbolic"), Search("Search", "rufin-search-symbolic"),
    Library("Library", "rufin-library-symbolic"), Saved("Saved", "rufin-bookmark-symbolic"),
}

/** Phones under 600 dp; a compact icon sidebar to 840 dp; the full sidebar beyond. */
private enum class ShellWidth { Phone, Medium, Expanded }

/** A detail page's scroll, for its top bar: how far it went, and whether Find is open. */
private class DetailBarState {
    var offset by mutableFloatStateOf(0f)
    var finding by mutableStateOf(false)
}

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
                        Modifier.size(40.dp).clip(CoverShape))
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
    val browse = model.browse
    val player = rememberPlayerConnection(model)
    FeedbackToast(model, player)
    val connectState = model.connect.state
    if (model.connect.operationActive && connectState != null && connectState.pairing == null && (connectState.connecting || model.connect.visibleError != null)) {
        BackHandler { model.connect.dismissOperation() }
        ConnectProgressScreen(model)
        return
    }
    SideEffect { model.controlNotifications = preferences.applicationSettings?.controlNotificationsEnabled != false }
    val configuration = LocalConfiguration.current
    val width = when {
        configuration.screenWidthDp < 600 -> ShellWidth.Phone
        configuration.screenWidthDp < 840 -> ShellWidth.Medium
        else -> ShellWidth.Expanded
    }
    // Split views need room for two panes; flip covers get dots for routes and one column.
    val split = configuration.screenWidthDp >= 600 && configuration.screenHeightDp >= 480
    val tiny = configuration.screenWidthDp < 420 && configuration.screenHeightDp < 420
    val short = configuration.screenHeightDp < 600
    // A phone on its side: a narrower sidebar that holds the player, as Spotify's does.
    val sidePlayer = width == ShellWidth.Expanded && configuration.screenHeightDp < 480
    var sources by rememberSaveable { mutableStateOf(false) }
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
    var sleepTimer by rememberSaveable { mutableStateOf(false) }
    var menu by remember { mutableStateOf<BrowseItem?>(null) }
    // The playing track's menu from the player, with the player's row on top.
    var playerMenu by remember { mutableStateOf<BrowseItem?>(null) }
    var playlist by remember { mutableStateOf<BrowseItem?>(null) }
    var sharing by remember { mutableStateOf<Pair<BrowseItem, Share>?>(null) }
    var createPlaylist by rememberSaveable { mutableStateOf(false) }
    var createMenu by remember { mutableStateOf(false) }
    var selectingPins by remember { mutableStateOf(false) }
    LaunchedEffect(browse.section) { if (browse.section != AppSection.Pins) selectingPins = false }
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
            }
        }
    }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    var moreStack by rememberSaveable { mutableStateOf(listOf(MoreDestination.Root)) }
    val moreDestination = moreStack.last()
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    fun closeDrawer() { scope.launch { drawer.close() } }
    fun openMore(destination: MoreDestination) {
        moreStack = generateSequence(destination) { it.parent }.toList().reversed()
        browse.section = AppSection.More
        closeDrawer()
    }
    LaunchedEffect(model.more.startupPeriod) {
        if (model.more.startupPeriod != null) {
            model.more.openStartupOverview()
            openMore(MoreDestination.Overview)
        }
    }
    val routeState = rememberSaveableStateHolder()
    val detailState = rememberSaveableStateHolder()
    val rootPages = browse.routes.filter { route ->
        route.id != "Search" && (route.id == browse.rootId || browse.configuredRoutes.any { it.id == route.id })
    }.associate { route -> key(route.id) {
        val options = remember(route.id) { browse.optionsFor(route.id) }
        val location = remember(route.id, route.route) { BrowseLocation(route.route, route.title, options) }
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
        val location = BrowseLocation(Route.Home, "Pins", options)
        // Always read: the sidebar lists the Saved entries as well as the Saved tab.
        BrowsePage(model, browse, location, options, pinsLifetime, pins = true).also { it.active = true }
    }
    LaunchedEffect(pinsPage) { pinsPage.observe() }
    val hasMedia by remember(player) { derivedStateOf { player.currentId?.mediaUri != null } }
    LaunchedEffect(translation) { browse.refreshLabels(translation) }
    // The Library tab returns to the route it last showed.
    val libraryRoutes = sidebarRoutes(browse).filter { it.id != "Home" }
    var libraryRoot by rememberSaveable { mutableStateOf<String?>(null) }
    LaunchedEffect(browse.rootId) { if (browse.rootId != "Home") libraryRoot = browse.rootId }
    val tab = when (browse.section) {
        AppSection.Search -> ShellTab.Search
        AppSection.Pins -> ShellTab.Saved
        AppSection.More -> null
        AppSection.Browse -> if (browse.rootId == "Home") ShellTab.Home else ShellTab.Library
    }
    fun leaveInput() { keyboard?.hide(); focusManager.clearFocus(); searchExpanded = false }
    fun selectRoute(id: String) {
        leaveInput()
        if (id == "Search") { browse.searchText = ""; browse.section = AppSection.Search }
        else { browse.chooseRoot(id); browse.section = AppSection.Browse }
        closeDrawer()
    }
    fun selectTab(selected: ShellTab) {
        leaveInput()
        when (selected) {
            ShellTab.Home -> { browse.chooseRoot("Home"); browse.section = AppSection.Browse }
            ShellTab.Library -> {
                val root = libraryRoot?.takeIf { id -> libraryRoutes.any { it.id == id } } ?: libraryRoutes.firstOrNull()?.id
                if (root != null) { browse.chooseRoot(root); browse.section = AppSection.Browse }
            }
            ShellTab.Search -> {
                if (tab == ShellTab.Search) focusSearch++ else { browse.searchText = ""; browse.section = AppSection.Search }
            }
            ShellTab.Saved -> browse.section = AppSection.Pins
        }
    }
    BackHandler(drawer.isOpen || searchExpanded || browse.detail || browse.section != AppSection.Browse || browse.rootId != "Home") {
        when {
            drawer.isOpen -> closeDrawer()
            browse.section == AppSection.More && moreStack.size > 1 -> moreStack = moreStack.dropLast(1)
            searchExpanded -> searchExpanded = false
            browse.section == AppSection.Browse && browse.detail -> browse.back()
            else -> selectTab(ShellTab.Home)
        }
    }
    // Scrolling down folds the bottom bar and the mini player into one row; up unfolds it.
    // Short screens start folded, also when a flip phone closes onto its cover screen.
    val dockedState = rememberSaveable(short) { mutableStateOf(short) }
    var docked by dockedState
    val reveal = remember { androidx.compose.animation.core.Animatable(if (fullPlayer) 1f else 0f) }
    LaunchedEffect(fullPlayer) {
        reveal.animateTo(if (fullPlayer) 1f else 0f, if (reduceMotion) tween(0)
            else androidx.compose.animation.core.spring(dampingRatio = .86f, stiffness = androidx.compose.animation.core.Spring.StiffnessMediumLow))
    }
    val screenHeight = with(LocalDensity.current) { configuration.screenHeightDp.dp.toPx() }
    val onReveal: (Float) -> Unit = { delta -> scope.launch { reveal.snapTo((reveal.value - delta / screenHeight).coerceIn(0f, 1f)) } }
    val onRevealEnd: (Boolean) -> Unit = { opened ->
        if (opened || reveal.value > .25f) fullPlayer = true else scope.launch { reveal.animateTo(0f) }
    }
    val density = LocalDensity.current
    // Made again with the state it writes: a change of screen height makes a new one.
    val dockScroll = remember(density, dockedState) {
        val threshold = with(density) { 48.dp.toPx() }
        object : NestedScrollConnection {
            var travel = 0f
            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
                if (consumed.y == 0f) return Offset.Zero
                travel = if (consumed.y < 0 == travel < 0) travel + consumed.y else consumed.y
                if (travel < -threshold) dockedState.value = true else if (travel > threshold) dockedState.value = false
                return Offset.Zero
            }
        }
    }
    // Home's bar is clear over its band and takes the surface once the band has scrolled away.
    var homeOffset by remember { mutableFloatStateOf(0f) }
    LaunchedEffect(browse.rootId, browse.section) { homeOffset = 0f }
    val homeScroll = remember {
        object : NestedScrollConnection {
            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
                homeOffset = (homeOffset - consumed.y).coerceAtLeast(0f)
                return Offset.Zero
            }
        }
    }
    val phone = width == ShellWidth.Phone
    val dockHeight = when {
        !phone -> if (hasMedia && !sidePlayer) 84.dp else 0.dp
        docked -> 72.dp
        else -> 64.dp + if (hasMedia) 60.dp else 0.dp
    }
    val sidebarActions = remember(browse) {
        SidebarActions(route = { selectRoute(it) }, saved = { item ->
            browse.section = AppSection.Browse; browse.open(item); closeDrawer()
        }, more = { openMore(it) }, addSource = { editSourceId = null; sources = true; closeDrawer() })
    }
    // Home and the detail pages draw their band under a transparent top bar.
    // In a split view the detail pane draws its own bar, and the list keeps the shell's.
    val overlay = browse.section == AppSection.Browse && (browse.rootId == "Home" || browse.detail && !split)
    val barHeight = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + 56.dp
    BrowseGestureSettings(preferences.swipeLeft, preferences.swipeRight, browse, { playlist = it }, { item, share -> sharing = item to share }) {
    ModalNavigationDrawer(drawerContent = {
        ModalDrawerSheet(Modifier.width(304.dp)) { Sidebar(model, browse, player, pinsPage, sidebarActions) }
    }, drawerState = drawer, gesturesEnabled = drawer.isOpen) {
    // The Surface gives the shell its background and its text colour.
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
    Row(Modifier.fillMaxSize()) {
        when (width) {
            ShellWidth.Medium -> SidebarRail(browse, { scope.launch { drawer.open() } }, ::selectRoute,
                { selectTab(ShellTab.Search) }, { selectTab(ShellTab.Saved) })
            ShellWidth.Expanded -> PermanentDrawerSheet(Modifier.width(if (sidePlayer) 240.dp else 300.dp).statusBarsPadding()) {
                val withSearch = remember(sidebarActions) {
                    SidebarActions(sidebarActions.route, sidebarActions.saved, sidebarActions.more, sidebarActions.addSource) { selectTab(ShellTab.Search) }
                }
                Sidebar(model, browse, player, pinsPage, withSearch, Modifier.weight(1f))
                if (sidePlayer && hasMedia) SidebarPlayer(player, { fullPlayer = true }, { outputs = true },
                    Modifier.navigationBarsPadding().padding(start = 8.dp, end = 8.dp, bottom = 8.dp))
            }
            ShellWidth.Phone -> {}
        }
    Box(Modifier.weight(1f).fillMaxHeight()) {
        CompositionLocalProvider(LocalPlayerBottomPadding provides dockHeight, LocalTinyScreen provides tiny) {
        Column(Modifier.fillMaxSize()) {
            if (!overlay) ShellTopBar(browse, moreDestination, searchExpanded, searchFocus, phone,
                onMenu = { scope.launch { drawer.open() } },
                onBack = { if (moreStack.size > 1) moreStack = moreStack.dropLast(1) else selectTab(ShellTab.Home) },
                onSearch = { if (searchExpanded) leaveInput() else { searchExpanded = true; focusSearch++ } },
                onSubmit = { if (browse.section == AppSection.Search) browse.submitSearch(); leaveInput() },
                onExpand = { searchExpanded = true }) {
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
                if (browse.section == AppSection.Browse) PlayerIconButton("rufin-filter-symbolic", "Customize", { sortPage = browse.visiblePage; sort = true })
                if (browse.section == AppSection.Pins) PlayerIconButton("rufin-document-edit-symbolic", "Select", { selectingPins = !selectingPins },
                    selected = selectingPins)
            }
            val refreshState = rememberPullToRefreshState()
            val refreshing = (model.sourceOperation as? SourceOperation.Refreshing)?.let {
                it.sourceId == model.sources?.selectedSourceId
            } == true
            // Pull-to-refresh stays on route roots; on detail pages a pull shows the Find field.
            val canRefresh = model.ready && model.sources?.selectedSourceId != null &&
                browse.section == AppSection.Browse && !browse.detail && !fullPlayer
            Box(Modifier.weight(1f).nestedScroll(dockScroll).nestedScroll(homeScroll).pullToRefresh(isRefreshing = refreshing, state = refreshState,
                enabled = canRefresh && !model.sourceBusy, onRefresh = model::refresh)) {
                // Each side of a transition shows the root it was composed for.
                val rootContent: @Composable (String, Boolean) -> Unit = { root, shown ->
                    CompositionLocalProvider(LocalOverlayBarHeight provides if (root == "Home") barHeight else 0.dp) {
                        RootContent(model, browse, player, root, rootPages, routeState, libraryRoutes, tiny, shown && !fullPlayer,
                            onMenu = { menu = it }, onPlaylist = { playlist = it }, onAddSource = { editSourceId = null; sources = true },
                            onRoutes = { openMore(MoreDestination.Routes) }, onSelect = ::selectRoute)
                    }
                }
                if (browse.section == AppSection.Browse) {
                    val top = browse.details.lastOrNull()
                    if (split && top != null) Row(Modifier.fillMaxSize()) {
                        // The list beside the detail: the root, or the detail it was opened from.
                        Box(Modifier.weight(.42f).fillMaxHeight()) {
                            val parent = browse.details.getOrNull(browse.details.size - 2)
                            if (parent == null) rootContent(browse.rootId, false)
                            else detailState.SaveableStateProvider("${browse.details.size - 1}:${parent.route}") {
                                BrowsePageContent(model, browse, player, parent, parent.options, false,
                                    { menu = it }, { playlist = it }, {}, owner = detailPages[parent])
                            }
                        }
                        VerticalDivider()
                        Box(Modifier.weight(.58f).fillMaxHeight()) {
                            // Under the shell's bar, the pane's own bar takes no status-bar inset.
                            DetailPane(model, browse, player, top, browse.details.size, detailPages[top], detailState, 56.dp, !fullPlayer,
                                { menu = it }, { playlist = it }, { sortPage = it; sort = true })
                        }
                    } else AnimatedContent(
                        targetState = BrowseDestination(browse.rootId, browse.details.size, top), modifier = Modifier.fillMaxSize(),
                        label = "libraryNavigation", transitionSpec = {
                            if (initialState.depth == 0 && targetState.depth == 0)
                                fadeIn(tween(if (reduceMotion) 0 else 180)) togetherWith fadeOut(tween(if (reduceMotion) 0 else 180))
                            else pageMotion(targetState.depth > initialState.depth, reduceMotion)
                        }, contentKey = { it.location ?: if (it.root == "Home") "Home" else "Library" },
                    ) { visible ->
                        val location = visible.location
                        if (location == null) rootContent(visible.root, visible.root == browse.rootId)
                        else DetailPane(model, browse, player, location, visible.depth, detailPages[location], detailState, barHeight,
                            !fullPlayer && browse.details.lastOrNull() === location, { menu = it }, { playlist = it }, { sortPage = it; sort = true })
                    }
                    val homeBar by animateColorAsState(if (homeOffset > with(density) { 160.dp.toPx() }) MaterialTheme.colorScheme.surface
                        else Color.Transparent, label = "homeBar")
                    val overBand = homeOffset <= with(density) { 160.dp.toPx() }
                    if (browse.rootId == "Home" && !browse.detail) OverlayBar(homeBar, onBand = overBand) {
                        if (phone) PlayerIconButton("rufin-open-menu-symbolic", "Open sidebar", { scope.launch { drawer.open() } })
                        Text(label("Home"), Modifier.weight(1f).padding(start = if (phone) 2.dp else 16.dp), maxLines = 1,
                            style = MaterialTheme.typography.titleLarge)
                    }
                } else Box(Modifier.fillMaxSize()) {
                    when (browse.section) {
                        AppSection.More -> routeState.SaveableStateProvider("section:More") {
                            MoreContent(model, preferences, player, browse, moreDestination, { moreStack = moreStack + it },
                                onAddSource = { editSourceId = null; sources = true }, onEditSource = { editSourceId = it; sources = true },
                                onOpen = onOpen, onSaveLog = onSaveLog, onOutput = { outputs = true }, navigationDepth = moreStack.size)
                        }
                        AppSection.Search -> SearchContent(model, preferences, browse, player, { menu = it }, { playlist = it }, {})
                        AppSection.Pins -> routeState.SaveableStateProvider("section:Pins") { PinsContent(model, browse, player, pinsPage, selectingPins) { selectingPins = false } }
                        AppSection.Browse -> {}
                    }
                }
                // The spinner follows the pull only; the refresh's progress is the toast's.
                if (canRefresh) PullToRefreshDefaults.Indicator(state = refreshState, isRefreshing = false,
                    modifier = Modifier.align(Alignment.TopCenter).padding(top = if (overlay) barHeight else 0.dp))
            }
        }
        }
        // Connect, startup errors and source progress are toasts over the content, above the bar,
        // so they never take the status bar or push the page's own bar down.
        Column(Modifier.align(Alignment.BottomCenter).navigationBarsPadding().padding(bottom = dockHeight + 8.dp)
            .padding(horizontal = 16.dp).widthIn(max = 480.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            val toast: @Composable (@Composable ColumnScope.() -> Unit) -> Unit = { content ->
                Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    shadowElevation = 4.dp) { Column(content = content) }
            }
            if (!model.connect.operationActive && connectState?.let { it.connecting } == true) toast {
                Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(label("Rufin Connect"), Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                    TextButton(model.connect::showOperation) { Text(label("Open Rufin Connect")) }
                }
            }
            model.startupError?.let { failure -> toast {
                Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
                    ErrorNotice(failure)
                    FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                        TextButton(model::retryStartup) { Text(label("Retry")) }
                    }
                }
            } }
            // Pages that list the library wait for a switch, and the launch reopening it, with their own spinner.
            model.sourceStatus?.takeUnless { model.sourceOperation is SourceOperation.Downloading || model.sourceOperation is SourceOperation.Switching }?.let { status -> toast {
                Text(status, Modifier.padding(horizontal = 16.dp, vertical = 10.dp), style = MaterialTheme.typography.bodyMedium)
                val progress = model.sourceProgress
                val total = progress?.total
                if (progress != null && total != null && total > 0UL) {
                    LinearProgressIndicator(progress = {
                        (progress.completed.toFloat() / total.toFloat()).coerceIn(0f, 1f)
                    }, modifier = Modifier.fillMaxWidth())
                } else LinearProgressIndicator(Modifier.fillMaxWidth())
            } }
        }
        Box(Modifier.align(Alignment.BottomCenter)) {
            if (phone) PhoneDock(tab, docked, hasMedia, player, ::selectTab, { docked = false },
                { fullPlayer = true }, { outputs = true }, onReveal, onRevealEnd)
            else if (hasMedia && !sidePlayer) MiniPlayer(player, { fullPlayer = true }, { outputs = true },
                Modifier.navigationBarsPadding().padding(start = 12.dp, end = 12.dp, bottom = 8.dp), onReveal = onReveal, onRevealEnd = onRevealEnd)
        }
    }
    }
    }
    }
    }
    // The player rises with the finger from the mini player and settles with a spring.
    if (fullPlayer || reveal.value > 0f) Box(Modifier.fillMaxSize().graphicsLayer {
        translationY = (1f - reveal.value) * size.height
        alpha = (reveal.value * 3f).coerceAtMost(1f)
    }) {
        PlayerScreen(player, preferences, onSettings = { destination -> fullPlayer = false; openMore(destination) }, onNavigate = { route, title, _ ->
            fullPlayer = false
            browse.section = AppSection.Browse
            browse.openRoute(route, title)
        }, onMore = {
            player.currentItem?.let { song -> browse.library?.trackItem(song)?.let { playerMenu = BrowseItem(it, 0UL, null) } }
        }, tiny = tiny) { fullPlayer = false }
    }
    if (outputs) PlayerOutputSheet(player) { outputs = false }
    if (sort) BrowseSortSheet(browse, { sort = false }, sortPage)
    if (random) RandomPlaySheet(model, browse) { random = false }
    if (sleepTimer) SleepTimerSheet(player) { sleepTimer = false }
    (playerMenu ?: menu)?.let { item ->
        val close = { menu = null; playerMenu = null }
        BrowseMenuSheet(model, browse, item, player, close, { playlist = item; close() }, { metadata = item; close() },
            onNavigate = { route, title, _ -> close(); fullPlayer = false; browse.section = AppSection.Browse; browse.openRoute(route, title) },
            top = if (playerMenu == null) null else { {
                PlayerMenuActions(player, onRandom = { close(); random = true }, onSleepTimer = { close(); sleepTimer = true },
                    onAutoDjSettings = { close(); fullPlayer = false; openMore(MoreDestination.Playback) })
            } })
    }
    playlist?.let { item -> PlaylistPickerSheet(model, browse, item, player) { playlist = null } }
    sharing?.let { (item, share) -> BrowseShareSheet(player, item, share) { sharing = null } }
    if (createPlaylist) PlaylistCreateSheet(model, { createPlaylist = false }) { name, sourceId, public ->
        io.github.screwys.rufin.shared.library.MemberSelection.fromUris(emptyList()).use { selection ->
            val source = model.sourceOwner ?: return@PlaylistCreateSheet
            io.github.screwys.rufin.shared.rufin_core.createPlaylist(source, sourceId, name, selection, public)?.let { created ->
                playlistFolder?.let { folder ->
                    created.playlistKey?.let { key -> movePlaylistToFolder(source,
                        io.github.screwys.rufin.shared.library.PlaylistFolderMember.Playlist(key), folder.key) }
                }
                created.settingsError?.let { error(it) }
            }
        }
        createPlaylist = false
    }
    if (createFolder) PlaylistFolderNameDialog(model, smartPlaylistList, { createFolder = false })
    if (createSmartPlaylist) SmartPlaylistEditor(model, browse, { createSmartPlaylist = false }, folder = playlistFolder?.key)
    metadata?.row?.metadataItem?.let { item -> MetadataEditorScreen(model, item) { metadata = null } }
    if (metadata == null && menu == null && playlist == null && !createPlaylist && !createFolder && !createSmartPlaylist && !sources && !outputs && !sort && !random) {
        ConnectPairingPrompt(model) { openMore(MoreDestination.Connect) }
    }
    if (sources) Dialog(onDismissRequest = { model.closeSourceSetup(); sources = false },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        SourceSetupScreen(model, { model.closeSourceSetup(); sources = false }, editSourceId)
    }
}

/** Flip covers and other tiny screens: route dots, one-column lists. */
internal val LocalTinyScreen = compositionLocalOf { false }

/** The top bar of route roots, Search, Saved and the pages of the ☰ menu: route tools live here. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ShellTopBar(browse: BrowseConnection, more: MoreDestination, searchExpanded: Boolean, searchFocus: FocusRequester,
    phone: Boolean, onMenu: () -> Unit, onBack: () -> Unit, onSearch: () -> Unit, onSubmit: () -> Unit, onExpand: () -> Unit,
    actions: @Composable RowScope.() -> Unit,
) {
    val searching = browse.section == AppSection.Search
    TopAppBar(expandedHeight = 56.dp, navigationIcon = {
        when {
            browse.section == AppSection.More && more != MoreDestination.Root ->
                PlayerIconButton("rufin-go-previous-symbolic", "Back", onBack)
            phone -> PlayerIconButton("rufin-open-menu-symbolic", "Open sidebar", onMenu)
        }
    }, title = {
        if (searchExpanded || searching) BasicTextField(
            modifier = Modifier.fillMaxWidth().focusRequester(searchFocus).onFocusChanged { if (it.isFocused) onExpand() },
            value = if (searching) browse.searchText else browse.filter,
            onValueChange = { if (searching) browse.searchText = it else browse.filter = it },
            singleLine = true, textStyle = MaterialTheme.typography.titleMedium.copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary), keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { onSubmit() }),
            decorationBox = { input -> Box(Modifier.padding(horizontal = 8.dp, vertical = 8.dp)) {
                if ((if (searching) browse.searchText else browse.filter).isBlank())
                    Text(translate(if (searching) "Search everything" else "Search"), style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                input()
            } })
        else Text(when (browse.section) {
            AppSection.Browse -> translate("Library")
            AppSection.Search -> translate("Search")
            AppSection.Pins -> translate("Saved")
            AppSection.More -> translate(more.title)
        }, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleLarge)
    }, actions = {
        if (browse.section == AppSection.Browse) PlayerIconButton(if (searchExpanded) "rufin-window-close-symbolic" else "rufin-search-symbolic",
            if (searchExpanded) "Close" else "Search", onSearch)
        actions()
    })
}

/** Home or the Library tab's routes: route tabs under the bar, and a swipe moves between routes. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RootContent(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection, root: String,
    pages: Map<String, BrowsePage>, routeState: androidx.compose.runtime.saveable.SaveableStateHolder,
    routes: List<AndroidRouteDescriptor>, tiny: Boolean, active: Boolean,
    onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit, onAddSource: () -> Unit, onRoutes: () -> Unit,
    onSelect: (String) -> Unit,
) {
    if (browse.library == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator(Modifier.semantics { contentDescription = translate("Loading...") })
        }
        return
    }
    if (model.sources?.sources?.isEmpty() == true && root !in listOf("History", "Playlists", "SmartPlaylists")) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
            RufinIcon("rufin-library-symbolic", null, Modifier.size(48.dp))
            Text(translate("No sources configured"), Modifier.padding(16.dp), style = MaterialTheme.typography.titleMedium)
            Button(onAddSource) { Text(translate("Add Source")) }
        }
        return
    }
    val page: @Composable (AndroidRouteDescriptor, Boolean) -> Unit = { route, shown ->
        val options = remember(route.id) { browse.optionsFor(route.id) }
        val location = remember(route.id, route.route) { BrowseLocation(route.route, route.title, options) }
        routeState.SaveableStateProvider(route.id) {
            BrowsePageContent(model, browse, player, location, options, shown, onMenu, onPlaylist, {}, owner = pages[route.id])
        }
    }
    if (root == "Home") {
        browse.routes.firstOrNull { it.id == "Home" }?.let { page(it, active) }
        return
    }
    if (routes.isEmpty()) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
            RufinIcon("rufin-library-symbolic", null, Modifier.size(48.dp), MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onRoutes) { Text(translate("Library")) }
        }
        return
    }
    val index = routes.indexOfFirst { it.id == root }.coerceAtLeast(0)
    val pager = rememberPagerState(index) { routes.size }
    // A tab, the sidebar or the Library tab picks the page; a swipe picks the route.
    LaunchedEffect(index) { if (pager.currentPage != index) pager.animateScrollToPage(index) }
    LaunchedEffect(pager, routes) {
        snapshotFlow { pager.settledPage }.collect { settled ->
            routes.getOrNull(settled)?.id?.let { id -> if (browse.rootId != "Home" && browse.rootId != id) browse.chooseRoot(id) }
        }
    }
    Column(Modifier.fillMaxSize()) {
        val position = remember { derivedStateOf { pager.currentPage + pager.currentPageOffsetFraction } }
        if (tiny) RouteDots(routes.size, position.value)
        else BrowseRouteTabs(routes, index, position, onSelect)
        HorizontalPager(pager, Modifier.weight(1f).fillMaxWidth(), key = { routes[it].id }, beyondViewportPageCount = 0) { shown ->
            page(routes[shown], active && shown == index)
        }
    }
}

@Composable
private fun RouteDots(count: Int, position: Float) {
    Row(Modifier.fillMaxWidth().height(20.dp), horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically) {
        repeat(count) { index ->
            val near = (1f - kotlin.math.abs(position - index)).coerceIn(0f, 1f)
            Box(Modifier.size(6.dp).clip(CircleShape).background(lerp(MaterialTheme.colorScheme.outlineVariant,
                MaterialTheme.colorScheme.primary, near)))
        }
    }
}

/** A detail page under its own bar: back and ⋯; a pull shows the Find field above the cover, and once the
 *  header scrolls away the bar takes the band's colour with the title and Play. */
@Composable
private fun DetailPane(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection, location: BrowseLocation,
    depth: Int, page: BrowsePage?, detailState: androidx.compose.runtime.saveable.SaveableStateHolder, barHeight: androidx.compose.ui.unit.Dp,
    active: Boolean, onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit, onCustomize: (BrowsePage) -> Unit,
) {
    val density = LocalDensity.current
    val state = remember(location) { DetailBarState() }
    val focus = remember { FocusRequester() }
    val scroll = remember(state) { object : NestedScrollConnection {
        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            state.offset = (state.offset - consumed.y).coerceAtLeast(0f)
            return Offset.Zero
        }
    } }
    LaunchedEffect(state.finding) { if (state.finding) focus.requestFocus() }
    val key = "$depth:${location.route}"
    val bandColor = remember(location) { mutableStateOf<Color?>(null) }
    DisposableEffect(location) { onDispose { if (location !in browse.details) detailState.removeState(key) } }
    Box(Modifier.fillMaxSize().nestedScroll(scroll)) {
        detailState.SaveableStateProvider(key) {
            Surface(Modifier.fillMaxSize()) {
                CompositionLocalProvider(LocalRowSwipes provides true, LocalOverlayBarHeight provides barHeight,
                    LocalDetailBand provides bandColor, LocalFinding provides state.finding,
                    LocalOpenFind provides { state.finding = true }) {
                    BrowsePageContent(model, browse, player, location, location.options, active, onMenu, onPlaylist, {}, owner = page)
                }
            }
        }
        val summary = page?.summary
        val past = state.offset > with(density) { 260.dp.toPx() }
        val band = bandColor.value ?: MaterialTheme.colorScheme.surface
        // Finding, the bar is the search view's own: the page's colour, the field, and back closes it.
        val color by animateColorAsState(if (state.finding) MaterialTheme.colorScheme.surface else if (past) band else Color.Transparent,
            label = "detailBar")
        val closeFind = { browse.filter = ""; state.finding = false }
        if (state.finding) BackHandler(onBack = closeFind)
        OverlayBar(color, inset = barHeight > 56.dp, onBand = bandColor.value != null && !state.finding) {
            PlayerIconButton("rufin-go-previous-symbolic", "Back", if (state.finding) closeFind else browse::back)
            Box(Modifier.weight(1f)) {
                if (state.finding) BasicTextField(browse.filter, { browse.filter = it }, Modifier.fillMaxWidth().focusRequester(focus),
                    singleLine = true, textStyle = MaterialTheme.typography.titleMedium.copy(color = LocalContentColor.current),
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    decorationBox = { input -> Box(Modifier.padding(horizontal = 4.dp, vertical = 8.dp)) {
                        if (browse.filter.isBlank()) Text(translate("Find in {name}").replace("{name}", summary?.row?.title ?: location.title),
                            maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium,
                            color = LocalContentColor.current.copy(alpha = .7f))
                        input()
                    } })
                else androidx.compose.animation.AnimatedVisibility(past, enter = fadeIn(), exit = fadeOut()) {
                    Text(summary?.row?.title ?: location.title, Modifier.padding(start = 4.dp), maxLines = 1,
                        overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                }
            }
            if (state.finding) { if (browse.filter.isNotEmpty()) PlayerIconButton("rufin-window-close-symbolic", "Clear", { browse.filter = "" }) }
            else if (past && page != null) PlayerIconButton("rufin-media-playback-start-symbolic", "Play",
                { page.play(QueuePlacement.Replace(0UL), false, summary?.row?.title ?: location.title) })
            // Customize as on other routes, and ⋯ opens the page's item menu; Find opens on a pull down.
            if (!state.finding) {
                if (page != null) PlayerIconButton("rufin-filter-symbolic", "Customize", { onCustomize(page) })
                summary?.let { shown -> PlayerIconButton("rufin-view-more-symbolic", "More actions", { onMenu(BrowseItem(shown.row, 0UL, null)) }) }
            }
        }
    }
}

/** A bar over a band: the status bar inset, then 56 dp of controls. */
@Composable
private fun OverlayBar(color: Color, inset: Boolean = true, onBand: Boolean = false, content: @Composable RowScope.() -> Unit) {
    Surface(Modifier.fillMaxWidth(), color = color, contentColor = if (onBand) OnBand else MaterialTheme.colorScheme.onSurface) {
        Row(Modifier.then(if (inset) Modifier.statusBarsPadding() else Modifier).height(56.dp).fillMaxWidth().padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically, content = content)
    }
}

/** The phone's bottom: the mini player over the tabs, or, scrolled down, one row of a tab circle,
 *  the mini player and a Search circle. */
@Composable
private fun PhoneDock(tab: ShellTab?, docked: Boolean, hasMedia: Boolean, player: PlayerConnection,
    onTab: (ShellTab) -> Unit, onUnfold: () -> Unit, onPlayer: () -> Unit, onOutput: () -> Unit,
    onReveal: (Float) -> Unit, onRevealEnd: (Boolean) -> Unit,
) {
    val reduceMotion = LocalReduceMotion.current
    AnimatedContent(docked, Modifier.fillMaxWidth(), label = "dock", transitionSpec = {
        (fadeIn(tween(if (reduceMotion) 0 else 220)) + slideInVertically(tween(if (reduceMotion) 0 else 260)) { it / 2 }) togetherWith
            (fadeOut(tween(if (reduceMotion) 0 else 160)) + slideOutVertically(tween(if (reduceMotion) 0 else 220)) { it / 2 })
    }) { folded ->
        if (folded) Row(Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            DockCircle((tab ?: ShellTab.Home).icon, "Show tabs", onUnfold)
            Box(Modifier.weight(1f)) { if (hasMedia) MiniPlayer(player, onPlayer, onOutput, Modifier, compact = true, onReveal = onReveal, onRevealEnd = onRevealEnd) }
            DockCircle(ShellTab.Search.icon, "Search", { onTab(ShellTab.Search) })
        } else Column(Modifier.fillMaxWidth()) {
            if (hasMedia) MiniPlayer(player, onPlayer, onOutput, onReveal = onReveal, onRevealEnd = onRevealEnd)
            // A slimmer bar than Material's 80 dp, as Spotify's.
            val inset = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
            NavigationBar(Modifier.fillMaxWidth().height(64.dp + inset)) {
                ShellTab.entries.forEach { entry ->
                    NavigationBarItem(tab == entry, { onTab(entry) },
                        icon = { RufinIcon(entry.icon, null, Modifier.size(24.dp)) }, label = { Text(translate(entry.title)) })
                }
            }
        }
    }
}

@Composable
private fun DockCircle(icon: String, label: String, onClick: () -> Unit) {
    Surface(onClick, Modifier.size(56.dp), shape = CircleShape, color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shadowElevation = 6.dp) {
        Box(contentAlignment = Alignment.Center) { RufinIcon(icon, translate(label), Modifier.size(24.dp)) }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun BrowseRouteTabs(routes: List<AndroidRouteDescriptor>, selected: Int, position: State<Float>, onSelect: (String) -> Unit) {
    PrimaryScrollableTabRow(selected.coerceAtLeast(0), Modifier.fillMaxWidth().height(44.dp),
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
                color = MaterialTheme.colorScheme.secondaryContainer, shape = RoundedCornerShape(16.dp))
        }) {
        routes.forEachIndexed { index, route ->
            val tint = lerp(MaterialTheme.colorScheme.onSurfaceVariant, MaterialTheme.colorScheme.onSecondaryContainer,
                (1f - kotlin.math.abs(position.value - index)).coerceIn(0f, 1f))
            Tab(index == selected, { onSelect(route.id) }, Modifier.height(44.dp),
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
    val page = owner ?: remember(location.route, options, pins) { BrowsePage(model, browse, location, options, scope, pins) }
    if (owner == null) LaunchedEffect(page) { page.observe() }
    DisposableEffect(page) { onDispose { page.active = false } }
    SideEffect { page.active = active; if (active) browse.visiblePage = page }
    val rows = page.pages.collectAsLazyPagingItems()
    if (!(page.summary != null && location.route.showsReleases) && page.homeSections.isEmpty() && rows.itemSnapshotList.items.isEmpty() &&
        (page.loading || rows.loadState.refresh is LoadState.Loading)) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val summary = page.summary
    val choices = when {
        location.route is Route.Favorites -> browse.categories
        location.route.showsReleases -> browse.categories.filter { it.category != CollectionCategory.ARTISTS }
        else -> emptyList()
    }
    val display = options.displaySettings ?: page.displaySettings ?: rows.itemSnapshotList.items.firstOrNull()?.displaySettings
    val covers = if (display != null) display.layout == LibraryLayout.GRID else page.covers
    Column(Modifier.fillMaxSize()) {
        if (location.route is Route.History) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                ChoiceChip(options.historySource == null, { options.historySource = null }, label = { Text(translate("All")) })
                // The source chip keeps the source it was picked with, as on the desktop.
                val source = options.historySource ?: model.sources?.selectedSourceId
                if (source != null) ChoiceChip(options.historySource != null, { options.historySource = source },
                    label = { Text(model.sourceName(source)) })
            }
            // History shows cards in Grid layout, as on the desktop.
            if (covers) Box(Modifier.weight(1f)) {
                BrowseContent(rows, emptyList(), player, false, location in browse.details, true,
                    browse::open, onMenu, { _, _ -> }, browse::favorite, onPlaylist, { if (active) onScrolled(it) },
                    displaySize = display?.size ?: DisplaySize.DEFAULT, gridSpacing = display?.gridSpacing ?: DisplaySize.DEFAULT,
                    onPlayItem = { item, shuffled -> browse.play(item, shuffled = shuffled) }, loading = page.loading,
                    onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) })
            } else HistoryRows(rows, page, player, browse, onMenu, onPlaylist, onScrolled)
            return@Column
        }
        if (location.route is Route.Folders) FolderNavigation(model, browse, page)
        if (summary != null) {
            val context = androidx.compose.ui.platform.LocalContext.current
            DetailContent(summary, page, rows, player, page.releaseGroups,
                browse::open, onMenu, { item, placement, shuffled -> page.play(placement, shuffled, item.row.title) }, browse::favorite,
                onPlaylist, { route, title -> browse.openRoute(route, title, summary.row.title) },
                onLink = { link -> model.runAction {
                    openMediaUri(context, link.url ?: page.sourceUri() ?: return@runAction)
                } }, onScrolled, onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) },
                onDownload = { browse.download(it, remove = it.row.downloaded) })
            return@Column
        }
        if (choices.isNotEmpty()) LazyRow(Modifier.fillMaxWidth(), contentPadding = PaddingValues(horizontal = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            itemsIndexed(choices, key = { _, option -> option.category.name }) { _, option ->
                ChoiceChip(options.category == option.category, { options.category = option.category; options.sort = null; options.displaySettings = null },
                    label = { Text(option.title) })
            }
        }
        Box(Modifier.weight(1f)) {
            BrowseContent(rows, page.homeSections, player, location.route is Route.Home, location in browse.details, covers,
                browse::open, onMenu, { section, shuffled -> page.playSection(section, shuffled) },
                browse::favorite, onPlaylist, { if (active) onScrolled(it) }, page.scrollSections,
                display?.size ?: DisplaySize.DEFAULT, display?.gridSpacing ?: DisplaySize.DEFAULT, { item, shuffled -> browse.play(item, shuffled = shuffled) }, page.fields,
                page.supportsLetterIndex, page::letterPosition, page.sortSelection?.descending ?: false,
                page.hasActiveFilters, page::resetFilters, page.loading, onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) },
                emptyIcon = if (location.route is Route.Folders) "rufin-folders-symbolic" else null,
                onStepShowcase = { forward -> browse.library?.stepShowcase(forward).orEmpty() })
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
    val fields = page.fields
    val images = fields.any { it.id == LibraryField.IMAGE || it.id == LibraryField.TITLE_MERGED }
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(top = 8.dp, bottom = 8.dp + LocalPlayerBottomPadding.current)) {
        items(rows.itemCount, key = rows.itemKey { "${it.row.key}:${it.index}" }) { index ->
            val item = rows[index]
            if (item == null) Spacer(Modifier.height(64.dp))
            else FieldRow(item.copy(displaySettings = page.options.displaySettings ?: page.displaySettings),
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
