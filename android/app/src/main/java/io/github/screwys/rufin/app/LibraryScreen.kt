package io.github.screwys.rufin.app

import io.github.screwys.rufin.ui.PageLoading

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
import io.github.screwys.rufin.player.LocalToolIconSize
import io.github.screwys.rufin.ui.SegmentedPill
import io.github.screwys.rufin.player.NowPlayingChip
import io.github.screwys.rufin.player.PlayerQueue
import io.github.screwys.rufin.ui.focusRing
import io.github.screwys.rufin.player.SidebarPlayer
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.more.MoreContent
import io.github.screwys.rufin.settings.IntegrationPage
import io.github.screwys.rufin.browse.PinsContent
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.rememberPlayerConnection
import io.github.screwys.rufin.player.PlayerScreen
import io.github.screwys.rufin.player.PlayerOutputSheet
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.sources.SourceSetupScreen
import io.github.screwys.rufin.sources.SourceWelcome
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
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.zIndex
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.foundation.focusGroup
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
import androidx.compose.ui.text.font.FontWeight
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
import io.github.screwys.rufin.ui.showPage
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

/** Phones under 600 dp; a compact icon sidebar to 840 dp; the full sidebar beyond; a TV's focus rail. */
private enum class ShellWidth { Phone, Medium, Expanded, Tv }

/** The routes whose entries open a page: on a large screen their list keeps the opened page beside it. */
private val Route.listsPages get() = this is Route.Albums || this is Route.Artists || this is Route.AlbumArtists ||
    this is Route.Genres || this is Route.Playlists || this is Route.SmartPlaylists

/** A detail page's scroll, for its top bar: how far it went, and whether Find is open. */
private class DetailBarState {
    var offset by mutableFloatStateOf(0f)
    var finding by mutableStateOf(false)
}

/** The feedback toast, `bottom` above the navigation bar: its own window, so it shows over sheets too, and
 *  no bigger than its card, so it never takes the taps meant for the bar under it. */
@Composable
internal fun FeedbackToast(model: RufinConnection, player: PlayerConnection, bottom: androidx.compose.ui.unit.Dp) {
    val feedback = model.feedback ?: return
    val lift = with(LocalDensity.current) { (bottom + 8.dp).roundToPx() + WindowInsets.navigationBars.getBottom(this) }
    androidx.compose.ui.window.Popup(alignment = Alignment.BottomCenter, offset = androidx.compose.ui.unit.IntOffset(0, -lift),
        properties = androidx.compose.ui.window.PopupProperties(focusable = false)) {
        Surface(Modifier.padding(horizontal = 16.dp).widthIn(max = 360.dp),
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
    val connectState = model.connect.state
    if (model.connect.operationActive && connectState != null && connectState.pairing == null && (connectState.connecting || model.connect.visibleError != null)) {
        BackHandler { model.connect.dismissOperation() }
        FeedbackToast(model, player, 0.dp)
        ConnectProgressScreen(model)
        return
    }
    SideEffect { model.controlNotifications = preferences.applicationSettings?.controlNotificationsEnabled != false }
    val configuration = LocalConfiguration.current
    val width = when {
        configuration.uiMode and android.content.res.Configuration.UI_MODE_TYPE_MASK ==
            android.content.res.Configuration.UI_MODE_TYPE_TELEVISION -> ShellWidth.Tv
        configuration.screenWidthDp < 600 -> ShellWidth.Phone
        configuration.screenWidthDp < 840 -> ShellWidth.Medium
        else -> ShellWidth.Expanded
    }
    // Flip covers get dots for routes and one column.
    val tiny = configuration.screenWidthDp < 420 && configuration.screenHeightDp < 420
    val short = configuration.screenHeightDp < 600
    // A phone on its side: a narrower sidebar that holds the player, as Spotify's does.
    val sidePlayer = width == ShellWidth.Expanded && configuration.screenHeightDp < 480
    var sources by rememberSaveable { mutableStateOf(false) }
    // The first run opens source setup by itself, once.
    var welcomed by rememberSaveable { mutableStateOf(false) }
    val noSources = model.sources?.sources?.isEmpty() == true
    LaunchedEffect(noSources) { if (noSources && !welcomed) { welcomed = true; sources = true } }
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
    // The route shows as a list beside the page picked from it.
    var splitShown by remember { mutableStateOf(false) }
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
    val select = remember { SelectMode() }
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
                            model.sourceOwner?.let { addToPlaylistFolder(it, listOf(member), folder.key) }
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
    var integration by rememberSaveable { mutableStateOf<IntegrationPage?>(null) }
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    // Closes once the page the drawer picked has composed, so building that page never uses up the slide.
    fun closeDrawer() { scope.launch { withFrameNanos { }; drawer.close() } }
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
    // Reads the routes and the section when a tab is picked, not when it was made: the dock can keep a
    // reference from a composition before the routes loaded.
    fun selectTab(selected: ShellTab) {
        leaveInput()
        when (selected) {
            ShellTab.Home -> { browse.chooseRoot("Home"); browse.section = AppSection.Browse }
            ShellTab.Library -> {
                val routes = sidebarRoutes(browse).filter { it.id != "Home" }
                val root = libraryRoot?.takeIf { id -> routes.any { it.id == id } } ?: routes.firstOrNull()?.id
                if (root != null) { browse.chooseRoot(root); browse.section = AppSection.Browse }
            }
            ShellTab.Search -> {
                if (browse.section == AppSection.Search) focusSearch++ else { browse.searchText = ""; browse.section = AppSection.Search }
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
    // How far the player is up: the finger moves it from the mini player and down from its top, and it
    // settles open or closed with a spring.
    val reveal = remember { androidx.compose.animation.core.Animatable(if (fullPlayer) 1f else 0f) }
    val settle: androidx.compose.animation.core.AnimationSpec<Float> = if (reduceMotion) tween(0)
        else androidx.compose.animation.core.spring(dampingRatio = .86f, stiffness = androidx.compose.animation.core.Spring.StiffnessMediumLow)
    // A D-pad or a keyboard moves into the player as it opens; touch focuses nothing.
    val playerFocus = remember { FocusRequester() }
    LaunchedEffect(fullPlayer) {
        if (fullPlayer) launch { withFrameNanos { }; runCatching { playerFocus.requestFocus() } }
        reveal.animateTo(if (fullPlayer) 1f else 0f, settle)
    }
    fun settlePlayer(open: Boolean) {
        if (open != fullPlayer) fullPlayer = open else scope.launch { reveal.animateTo(if (open) 1f else 0f, settle) }
    }
    val screenHeight = with(LocalDensity.current) { configuration.screenHeightDp.dp.toPx() }
    val onReveal: (Float) -> Unit = { delta -> scope.launch { reveal.snapTo((reveal.value - delta / screenHeight).coerceIn(0f, 1f)) } }
    val onRevealEnd: (Boolean) -> Unit = { opened -> settlePlayer(opened || reveal.value > .25f) }
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
    // The sidebar and a TV's rail pick the route; phones and TVs keep the route tabs.
    val tabs = phone || width == ShellWidth.Tv
    // A tablet's player floats over the page, as the foldable's does, and opens the queue beside the page.
    val tablet = width == ShellWidth.Expanded && !sidePlayer
    var queuePanel by rememberSaveable { mutableStateOf(false) }
    val dockHeight = when {
        tablet -> if (hasMedia) 100.dp else 0.dp
        width == ShellWidth.Medium -> if (hasMedia) 84.dp else 0.dp
        !phone -> 0.dp
        docked -> 72.dp
        else -> 64.dp + if (hasMedia) 60.dp else 0.dp
    }
    // Over the player it clears the player's own buttons at the bottom.
    FeedbackToast(model, player, if (fullPlayer) 64.dp else dockHeight)
    // The queue pane's opener ends the bar at the window's top right, with the desktop's right-sidebar icons.
    val queueToggle: @Composable () -> Unit = {
        if (tablet) PlayerIconButton(if (queuePanel) "rufin-sidebar-collapse-right-symbolic" else "rufin-sidebar-expand-right-symbolic",
            if (queuePanel) "Hide sidebar" else "Show sidebar", { queuePanel = !queuePanel })
    }
    val openPlayerMenu = { player.currentItem?.let { song -> browse.library?.trackItem(song)?.let { playerMenu = BrowseItem(it, 0UL, null) } }; Unit }
    val sidebarActions = remember(browse) {
        SidebarActions(route = { selectRoute(it) }, saved = { item ->
            browse.section = AppSection.Browse; browse.open(item); closeDrawer()
        }, more = { openMore(it) }, addSource = { editSourceId = null; sources = true; closeDrawer() })
    }
    val barHeight = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + 56.dp
    BrowseGestureSettings(preferences.swipeLeft, preferences.swipeRight, browse, { playlist = it }, { item, share -> sharing = item to share }) {
    ModalNavigationDrawer(drawerContent = {
        ModalDrawerSheet(Modifier.width(304.dp)) { Sidebar(model, browse, player, pinsPage, sidebarActions) }
    }, drawerState = drawer, gesturesEnabled = drawer.isOpen) {
    // The Surface gives the shell its background and its text colour. Under the open player it takes no
    // focus: a D-pad stays in the player.
    Surface(Modifier.fillMaxSize().focusProperties { onEnter = { if (fullPlayer) cancelFocusChange() } }.focusGroup(),
        color = MaterialTheme.colorScheme.background) {
    CompositionLocalProvider(LocalToolIconSize provides if (tablet) 20.dp else 24.dp) {
    Row(Modifier.fillMaxSize()) {
        when (width) {
            ShellWidth.Medium -> SidebarRail(browse, player, pinsPage, { scope.launch { drawer.open() } }, ::selectRoute,
                { selectTab(ShellTab.Search) }, { selectTab(ShellTab.Saved) }, sidebarActions.saved)
            ShellWidth.Tv -> TvRail(tab, ::selectTab, browse.section == AppSection.More) { openMore(MoreDestination.Root) }
            ShellWidth.Expanded -> PermanentDrawerSheet(Modifier.width(if (sidePlayer) 240.dp else 264.dp).statusBarsPadding()) {
                val withSearch = remember(sidebarActions) {
                    SidebarActions(sidebarActions.route, sidebarActions.saved, sidebarActions.more, sidebarActions.addSource) { selectTab(ShellTab.Search) }
                }
                Sidebar(model, browse, player, pinsPage, withSearch, Modifier.weight(1f))
                if (sidePlayer && hasMedia) SidebarPlayer(player, { fullPlayer = true }, { outputs = true },
                    Modifier.navigationBarsPadding().padding(start = 8.dp, end = 8.dp, bottom = 8.dp))
            }
            ShellWidth.Phone -> {}
        }
    BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
        val paneWidth = maxWidth
        // On large screens (a foldable from 640 dp, a tablet) a route that lists pages shows them as slim
        // rows on the left and the page picked from them on the right, empty until one is picked. That is
        // the default there; with Grid picked for the route it is a grid in one pane, and a page opens over
        // it, as on phones.
        val rootRoute = browse.routes.firstOrNull { it.id == browse.rootId }
        val rootPage = rootPages[browse.rootId]
        val split = paneWidth >= 556.dp && configuration.screenHeightDp >= 480 && browse.section == AppSection.Browse &&
            rootRoute?.route?.listsPages == true && (rootPage?.options?.displaySettings ?: rootPage?.displaySettings)?.gridPicked != true
        SideEffect { splitShown = split }
        CompositionLocalProvider(LocalPlayerBottomPadding provides dockHeight, LocalTinyScreen provides tiny,
            LocalSelectMode provides select) {
        // The route roots carry this bar, so it moves with its page instead of shifting the page beside it.
        val shellBar: @Composable () -> Unit = {
            ShellTopBar(browse, moreDestination, if (moreDestination == MoreDestination.Integration) integration?.name.orEmpty() else translate(moreDestination.title),
                if (tabs) null else rootRoute?.title, searchExpanded, searchFocus, tabs, !split,
                onMenu = { scope.launch { drawer.open() } },
                onBack = { if (moreStack.size > 1) moreStack = moreStack.dropLast(1) else selectTab(ShellTab.Home) },
                onSearch = { if (searchExpanded) leaveInput() else { searchExpanded = true; focusSearch++ } },
                onSubmit = { if (browse.section == AppSection.Search) browse.submitSearch(); leaveInput() },
                onExpand = { searchExpanded = true }) {
                if (width == ShellWidth.Tv && hasMedia) NowPlayingChip(player) { fullPlayer = true }
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
                    PlayerIconButton("rufin-selection-mode-symbolic", "Select", { select.route = (if (split) rootPage else browse.visiblePage)?.location?.route })
                    if (!smartPlaylistList) PlayerIconButton("rufin-document-open-symbolic", "Import Playlist", {
                        importPlaylist.launch(Intent(Intent.ACTION_OPEN_DOCUMENT).setType("*/*")
                            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION))
                    })
                }
                // Beside an opened page the bar is still its list's.
                if (browse.section == AppSection.Browse) PlayerIconButton("rufin-filter-symbolic", "Customize",
                    { sortPage = if (split) rootPage else browse.visiblePage; sort = true })
                if (browse.section == AppSection.Pins) PlayerIconButton("rufin-document-edit-symbolic", "Select", { selectingPins = !selectingPins },
                    selected = selectingPins)
                // Beside an opened page, that page's bar is the one at the window's edge.
                if (!split) queueToggle()
            }
        }
        // Home's bar is clear over the band at rest. Once the page moves under it, it draws the band where the
        // band stood, unchanged to the eye, so the cover never passes behind its text; past the band it fades
        // to the page's colour.
        val homeBand = remember { mutableStateOf<HomeBandLook?>(null) }
        val overBand = homeOffset <= with(density) { 160.dp.toPx() }
        val homeBar by animateColorAsState(if (overBand) Color.Transparent else MaterialTheme.colorScheme.surface, label = "homeBar")
        Column(Modifier.fillMaxSize()) {
            if (browse.section != AppSection.Browse) shellBar()
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
                        RootContent(model, browse, player, root, rootPages, routeState, libraryRoutes, tiny, tabs, split, shown && !fullPlayer,
                            onMenu = { menu = it }, onPlaylist = { playlist = it }, onAddSource = { editSourceId = null; sources = true },
                            onRoutes = { openMore(MoreDestination.Routes) }, onSelect = ::selectRoute)
                    }
                }
                if (browse.section == AppSection.Browse) {
                    val top = browse.details.lastOrNull()
                    if (split) Row(Modifier.fillMaxSize()) {
                        Column(Modifier.width((paneWidth * .4f).coerceIn(236.dp, 360.dp)).fillMaxHeight()) {
                            shellBar()
                            PaneSearch(browse.optionsFor(browse.rootId))
                            Box(Modifier.weight(1f)) {
                                CompositionLocalProvider(LocalSelectedRoute provides browse.details.firstOrNull()?.route, LocalListPane provides true) {
                                    rootContent(browse.rootId, true)
                                }
                            }
                        }
                        VerticalDivider()
                        // Empty until a row is picked.
                        Surface(Modifier.weight(1f).fillMaxHeight()) {
                            if (top != null) DetailPane(model, browse, player, top, browse.details.size, detailPages[top], detailState, barHeight,
                                !fullPlayer, { menu = it }, { playlist = it }, { sortPage = it; sort = true }, beside = true, trailing = queueToggle)
                            else Box(Modifier.fillMaxWidth().statusBarsPadding().height(56.dp).padding(horizontal = 4.dp),
                                contentAlignment = Alignment.CenterEnd) { queueToggle() }
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
                        val shown = visible.root == browse.rootId
                        if (location != null) DetailPane(model, browse, player, location, visible.depth, detailPages[location], detailState, barHeight,
                            !fullPlayer && browse.details.lastOrNull() === location, { menu = it }, { playlist = it }, { sortPage = it; sort = true },
                            trailing = queueToggle)
                        else if (visible.root == "Home") Box(Modifier.fillMaxSize()) {
                            CompositionLocalProvider(LocalHomeBand provides homeBand) { rootContent(visible.root, shown) }
                            OverlayBar(homeBar, onBand = overBand, modifier = Modifier.drawBehind {
                                if (homeOffset > 0f) homeBand.value?.let { band -> drawBand(band.stops, band.size) }
                            }) {
                                if (tabs) PlayerIconButton("rufin-open-menu-symbolic", "Open sidebar", { scope.launch { drawer.open() } })
                                Text(label("Home"), Modifier.weight(1f).padding(start = if (tabs) 2.dp else 16.dp), maxLines = 1,
                                    style = MaterialTheme.typography.titleLarge)
                                if (width == ShellWidth.Tv && hasMedia) NowPlayingChip(player) { fullPlayer = true }
                                PlayerIconButton("rufin-random-symbolic", "Play random", { random = true })
                                queueToggle()
                            }
                        } else Column(Modifier.fillMaxSize()) {
                            shellBar()
                            Box(Modifier.weight(1f)) { rootContent(visible.root, shown) }
                        }
                    }
                } else Box(Modifier.fillMaxSize()) {
                    when (browse.section) {
                        AppSection.More -> routeState.SaveableStateProvider("section:More") {
                            MoreContent(model, preferences, player, browse, moreDestination, { moreStack = moreStack + it },
                                integration, { page ->
                                    integration = page
                                    if (moreDestination != MoreDestination.Integration) moreStack = moreStack + MoreDestination.Integration
                                }, { if (moreStack.size > 1) moreStack = moreStack.dropLast(1) },
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
                    modifier = Modifier.align(Alignment.TopCenter).padding(top = barHeight))
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
            else if (width == ShellWidth.Medium && hasMedia) MiniPlayer(player, { fullPlayer = true }, { outputs = true },
                Modifier.navigationBarsPadding().padding(start = 16.dp, end = 16.dp, bottom = 8.dp), onReveal = onReveal, onRevealEnd = onRevealEnd)
            // Centred on the page and no wider than it needs: a player, not a bar.
            else if (tablet && hasMedia) CompositionLocalProvider(LocalToolIconSize provides 24.dp) {
                MiniPlayer(player, { fullPlayer = true }, { outputs = true },
                    Modifier.navigationBarsPadding().padding(start = 20.dp, end = 20.dp, bottom = 16.dp).widthIn(max = 640.dp),
                    onReveal = onReveal, onRevealEnd = onRevealEnd, floating = true)
            }
        }
    }
    if (tablet && queuePanel) {
        VerticalDivider()
        PlayerQueue(player, Modifier.width(280.dp).fillMaxHeight().statusBarsPadding(), nowPlaying = false,
            onNavigate = { route, title, _ -> browse.section = AppSection.Browse; browse.openRoute(route, title) })
    }
    }
    }
    }
    }
    }
    // The player rises with the finger from the mini player and settles with a spring; while it is up,
    // the page under it is shaded.
    if (fullPlayer || reveal.value > 0f) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.scrim.copy(alpha = .75f)))
        Box(Modifier.fillMaxSize().focusRequester(playerFocus).focusGroup().graphicsLayer {
            translationY = (1f - reveal.value) * size.height
            alpha = (reveal.value * 3f).coerceAtMost(1f)
        }) {
            PlayerScreen(player, preferences, onSettings = { destination -> fullPlayer = false; openMore(destination) }, onNavigate = { route, title, _ ->
                fullPlayer = false
                browse.section = AppSection.Browse
                browse.openRoute(route, title)
            }, onMore = openPlayerMenu, tiny = tiny, onPull = onReveal, onPullEnd = { close -> settlePlayer(!close) }) { fullPlayer = false }
        }
    }
    if (outputs) PlayerOutputSheet(player) { outputs = false }
    if (sort) BrowseSortSheet(browse, { sort = false }, sortPage, beside = splitShown && sortPage != null && sortPage === rootPages[browse.rootId])
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
                    created.playlistKey?.let { key -> addToPlaylistFolder(source,
                        listOf(io.github.screwys.rufin.shared.library.PlaylistFolderMember.Playlist(key)), folder.key) }
                }
                created.settingsError?.let { error(it) }
            }
        }
        createPlaylist = false
    }
    if (createFolder) PlaylistFolderNameDialog(model, { createFolder = false })
    if (createSmartPlaylist) SmartPlaylistEditor(model, browse, { createSmartPlaylist = false }, folder = playlistFolder?.key)
    metadata?.row?.metadataItem?.let { item -> MetadataEditorScreen(model, item) { metadata = null } }
    if (metadata == null && menu == null && playlist == null && !createPlaylist && !createFolder && !createSmartPlaylist && !sources && !outputs && !sort && !random) {
        ConnectPairingPrompt(model) { openMore(MoreDestination.Connect) }
    }
    if (sources) Dialog(onDismissRequest = { model.closeSourceSetup(); sources = false },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        val close = { model.closeSourceSetup(); sources = false }
        // Greets while there is no source; the pages keep it once the first one is being added.
        val welcome = remember(sources) {
            if (editSourceId != null || model.sources?.sources?.isEmpty() != true) null
            else SourceWelcome(preferences, player, importBackup = { close(); openMore(MoreDestination.Privacy) },
                join = { device -> close(); openMore(MoreDestination.Connect); model.connect.join(device) })
        }
        SourceSetupScreen(model, close, editSourceId, welcome = welcome)
    }
}

/** Flip covers and other tiny screens: route dots, one-column lists. */
internal val LocalTinyScreen = compositionLocalOf { false }

/** The top bar of route roots, Search, Saved and the pages of the ☰ menu: route tools live here. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ShellTopBar(browse: BrowseConnection, more: MoreDestination, moreTitle: String,
    /** The route's own name, where no tabs under the bar show it. */
    route: String?, searchExpanded: Boolean, searchFocus: FocusRequester,
    /** ☰ opens the sidebar where none is docked. */
    menu: Boolean,
    /** The route's filter opens in the bar, unless its pane keeps a field of its own. */
    search: Boolean, onMenu: () -> Unit, onBack: () -> Unit, onSearch: () -> Unit, onSubmit: () -> Unit, onExpand: () -> Unit,
    actions: @Composable RowScope.() -> Unit,
) {
    val searching = browse.section == AppSection.Search
    // The bar filters its route, whichever page is open beside it.
    val options = browse.optionsFor(browse.rootId)
    TopAppBar(expandedHeight = 56.dp, navigationIcon = {
        when {
            browse.section == AppSection.More && more != MoreDestination.Root ->
                PlayerIconButton("rufin-go-previous-symbolic", "Back", onBack)
            menu -> PlayerIconButton("rufin-open-menu-symbolic", "Open sidebar", onMenu)
        }
    }, title = {
        if (searchExpanded || searching) BasicTextField(
            modifier = Modifier.fillMaxWidth().focusRequester(searchFocus).onFocusChanged { if (it.isFocused) onExpand() },
            value = if (searching) browse.searchText else options.filter,
            onValueChange = { if (searching) browse.searchText = it else options.filter = it },
            singleLine = true, textStyle = MaterialTheme.typography.titleMedium.copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary), keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { onSubmit() }),
            decorationBox = { input -> Box(Modifier.padding(horizontal = 8.dp, vertical = 8.dp)) {
                if ((if (searching) browse.searchText else options.filter).isBlank())
                    Text(translate(if (searching) "Search everything" else "Search"), style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                input()
            } })
        else Text(when (browse.section) {
            AppSection.Browse -> route ?: translate("Library")
            AppSection.Search -> translate("Search")
            AppSection.Pins -> translate("Saved")
            AppSection.More -> moreTitle
        }, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleLarge)
    }, actions = {
        if (browse.section == AppSection.Browse && search) PlayerIconButton(if (searchExpanded) "rufin-window-close-symbolic" else "rufin-search-symbolic",
            if (searchExpanded) "Close" else "Search", onSearch)
        actions()
    })
}

/** Home or the Library tab's routes: route tabs under the bar, and a swipe moves between routes. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RootContent(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection, root: String,
    pages: Map<String, BrowsePage>, routeState: androidx.compose.runtime.saveable.SaveableStateHolder,
    routes: List<AndroidRouteDescriptor>, tiny: Boolean,
    /** The routes show as tabs under the bar. */
    tabs: Boolean,
    /** The list of a split view: a row opens its page beside the list, in place of the page open there. */
    beside: Boolean, active: Boolean,
    onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit, onAddSource: () -> Unit, onRoutes: () -> Unit,
    onSelect: (String) -> Unit,
) {
    if (browse.library == null) {
        PageLoading()
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
            if (beside) BrowsePageContent(model, browse, player, location, options, shown, onMenu, onPlaylist, {}, owner = pages[route.id],
                visible = !browse.detail, onOpen = { item -> if (item.row.route != null) browse.chooseRoot(browse.rootId); browse.open(item) })
            else BrowsePageContent(model, browse, player, location, options, shown, onMenu, onPlaylist, {}, owner = pages[route.id])
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
    // Without tabs the routes are not side by side: the sidebar's pick shows at once, with nothing to slide past.
    if (!tabs && !tiny) {
        page(routes[index], active)
        return
    }
    val pager = rememberPagerState(index) { routes.size }
    val fade = remember { androidx.compose.animation.core.Animatable(1f) }
    val reduceMotion = LocalReduceMotion.current
    // A tab, the sidebar or the Library tab picks the page; a swipe picks the route.
    LaunchedEffect(index) { if (pager.currentPage != index) pager.showPage(index, fade, reduceMotion) }
    LaunchedEffect(pager, routes) {
        snapshotFlow { pager.settledPage }.collect { settled ->
            routes.getOrNull(settled)?.id?.let { id -> if (browse.rootId != "Home" && browse.rootId != id) browse.chooseRoot(id) }
        }
    }
    Column(Modifier.fillMaxSize()) {
        val position = remember { derivedStateOf { pager.currentPage + pager.currentPageOffsetFraction } }
        if (tiny) RouteDots(routes.size, position.value)
        else BrowseRouteTabs(routes, index, position, onSelect)
        HorizontalPager(pager, Modifier.weight(1f).fillMaxWidth().graphicsLayer { alpha = fade.value }, key = { routes[it].id },
            beyondViewportPageCount = 0) { shown ->
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

/** The filter of a split view's list, kept in view over it. */
@Composable
private fun PaneSearch(options: BrowseOptions) {
    BasicTextField(options.filter, { options.filter = it }, Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, bottom = 8.dp),
        singleLine = true, textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary), decorationBox = { input ->
            Row(Modifier.height(44.dp).clip(RoundedCornerShape(22.dp)).background(MaterialTheme.colorScheme.surfaceContainerHigh)
                .padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RufinIcon("rufin-search-symbolic", null, Modifier.size(20.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                Box(Modifier.weight(1f)) {
                    if (options.filter.isEmpty()) Text(translate("Search"), style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                    input()
                }
                if (options.filter.isNotEmpty()) RufinIcon("rufin-window-close-symbolic", translate("Clear"),
                    Modifier.size(18.dp).clip(CircleShape).clickable { options.filter = "" })
            }
        })
}

/** A TV's focus rail: the four tabs, and Settings at its foot. */
@Composable
private fun TvRail(tab: ShellTab?, onTab: (ShellTab) -> Unit, settings: Boolean, onSettings: () -> Unit) {
    val entry: @Composable (String, String, Boolean, () -> Unit) -> Unit = { icon, title, picked, onClick ->
        Surface(onClick, Modifier.size(48.dp).focusRing(CircleShape), shape = CircleShape,
            color = if (picked) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent) {
            Box(contentAlignment = Alignment.Center) { RufinIcon(icon, translate(title), Modifier.size(24.dp)) }
        }
    }
    Column(Modifier.fillMaxHeight().width(80.dp).systemBarsPadding().padding(vertical = 16.dp), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp)) {
        ShellTab.entries.forEach { shown -> entry(shown.icon, shown.title, tab == shown) { onTab(shown) } }
        Spacer(Modifier.weight(1f))
        entry("rufin-preferences-system-symbolic", "Settings", settings, onSettings)
    }
}

/** A detail page under its own bar: back and ⋯; a pull shows the Find field above the cover, and once the
 *  header scrolls away the bar takes the band's colour with the title and Play. */
@Composable
private fun DetailPane(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection, location: BrowseLocation,
    depth: Int, page: BrowsePage?, detailState: androidx.compose.runtime.saveable.SaveableStateHolder, barHeight: androidx.compose.ui.unit.Dp,
    active: Boolean, onMenu: (BrowseItem) -> Unit, onPlaylist: (BrowseItem) -> Unit, onCustomize: (BrowsePage) -> Unit,
    /** Beside its list: back only leaves a page opened from this one, and Find has a button, since a pull
     *  is a touch gesture. */
    beside: Boolean = false,
    /** What ends the bar, after the page's own buttons. */
    trailing: @Composable () -> Unit = {},
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
    // A folder's page lists entries under no header of its own: its bar is solid, names it, and the list
    // starts under it.
    val plain = location.route is Route.Folders || location.route is Route.PlaylistFolder
    val bandColor = remember(location) { mutableStateOf<Color?>(null) }
    DisposableEffect(location) { onDispose { if (location !in browse.details) detailState.removeState(key) } }
    Box(Modifier.fillMaxSize().nestedScroll(scroll)) {
        detailState.SaveableStateProvider(key) {
            Surface(Modifier.fillMaxSize().padding(top = if (plain) barHeight else 0.dp)) {
                CompositionLocalProvider(LocalRowSwipes provides true, LocalOverlayBarHeight provides barHeight,
                    LocalDetailBand provides bandColor, LocalFinding provides state.finding,
                    LocalOpenFind provides { state.finding = true }, LocalBesideList provides beside) {
                    BrowsePageContent(model, browse, player, location, location.options, active, onMenu, onPlaylist, {}, owner = page)
                }
            }
        }
        val summary = page?.summary
        val past = state.offset > with(density) { 260.dp.toPx() }
        val band = bandColor.value ?: MaterialTheme.colorScheme.surface
        // Finding, the bar is the search view's own: the page's colour, the field, and back closes it.
        val color by animateColorAsState(if (state.finding || plain) MaterialTheme.colorScheme.surface else if (past) band else Color.Transparent,
            label = "detailBar")
        val closeFind = { browse.filter = ""; state.finding = false }
        if (state.finding) BackHandler(onBack = closeFind)
        OverlayBar(color, inset = barHeight > 56.dp, onBand = !state.finding && !plain && (!past || bandColor.value != null)) {
            if (!beside || depth > 1 || state.finding) PlayerIconButton("rufin-go-previous-symbolic", "Back", if (state.finding) closeFind else browse::back)
            // A playlist and a smart playlist open their Select mode from beside Back.
            if (!state.finding && (location.route is Route.PlaylistDetail && summary?.row?.playlist != null || location.route is Route.SmartPlaylistDetail))
                LocalSelectMode.current.let { select -> PlayerIconButton("rufin-selection-mode-symbolic", "Select", { select.route = location.route }) }
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
                else androidx.compose.animation.AnimatedVisibility(past || plain, enter = fadeIn(), exit = fadeOut()) {
                    Text(summary?.row?.title ?: location.title, Modifier.padding(start = 4.dp), maxLines = 1,
                        overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                }
            }
            if (state.finding) { if (browse.filter.isNotEmpty()) PlayerIconButton("rufin-window-close-symbolic", "Clear", { browse.filter = "" }) }
            else if (past && !plain && page != null) PlayerIconButton("rufin-media-playback-start-symbolic", "Play",
                { page.play(QueuePlacement.Replace(0UL), false, summary?.row?.title ?: location.title) })
            // Customize as on other routes, and ⋯ opens the page's item menu; Find opens on a pull down.
            if (!state.finding) {
                if (beside) PlayerIconButton("rufin-search-symbolic", "Search", { state.finding = true })
                // A folder's Select sits with its list tools, as on the playlist lists.
                if (location.route is Route.PlaylistFolder) LocalSelectMode.current.let { select ->
                    PlayerIconButton("rufin-selection-mode-symbolic", "Select", { select.route = location.route })
                }
                if (page != null) PlayerIconButton("rufin-filter-symbolic", "Customize", { onCustomize(page) })
                summary?.let { shown -> PlayerIconButton("rufin-view-more-symbolic", "More actions", { onMenu(BrowseItem(shown.row, 0UL, null)) }) }
                trailing()
            }
        }
    }
}

/** A bar over a band: the status bar inset, then 56 dp of controls. */
@Composable
private fun OverlayBar(color: Color, inset: Boolean = true, onBand: Boolean = false, modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit) {
    Surface(modifier.fillMaxWidth(), color = color, contentColor = if (onBand) OnBand else MaterialTheme.colorScheme.onSurface) {
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
                // The picked tab takes the accent, its label a heavier weight, as Navic's bar does.
                val colors = NavigationBarItemDefaults.colors(selectedIconColor = MaterialTheme.colorScheme.primary,
                    selectedTextColor = MaterialTheme.colorScheme.primary, indicatorColor = MaterialTheme.colorScheme.primaryContainer)
                ShellTab.entries.forEach { entry ->
                    val picked = tab == entry
                    NavigationBarItem(picked, { onTab(entry) }, colors = colors,
                        icon = { RufinIcon(entry.icon, null, Modifier.size(24.dp)) },
                        label = { Text(translate(entry.title), fontWeight = if (picked) FontWeight.Bold else FontWeight.Medium) })
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
            Tab(index == selected, { onSelect(route.id) }, Modifier.height(44.dp).focusRing(RoundedCornerShape(16.dp)),
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
    /** The page the bar's tools act on; a list beside a detail is shown without being it. */
    visible: Boolean = active,
    onOpen: (BrowseItem) -> Unit = browse::open,
) {
    val scope = rememberCoroutineScope()
    val page = owner ?: remember(location.route, options, pins) { BrowsePage(model, browse, location, options, scope, pins) }
    if (owner == null) LaunchedEffect(page) { page.observe() }
    DisposableEffect(page) { onDispose { page.active = false } }
    SideEffect { page.active = active; if (active && visible) browse.visiblePage = page }
    DisposableEffect(page, active) {
        if (active) browse.shownPages += page
        onDispose { browse.shownPages -= page }
    }
    val rows = page.pages.collectAsLazyPagingItems()
    if (!(page.summary != null && location.route.showsReleases) && page.homeSections.isEmpty() && rows.itemSnapshotList.items.isEmpty() &&
        (page.loading || rows.loadState.refresh is LoadState.Loading)) {
        PageLoading()
        return
    }
    val summary = page.summary
    val choices = when {
        location.route is Route.Favorites -> browse.categories
        location.route.showsReleases -> browse.categories.filter { it.category != CollectionCategory.ARTISTS }
        else -> emptyList()
    }
    val display = options.displaySettings ?: page.displaySettings ?: rows.itemSnapshotList.items.firstOrNull()?.displaySettings
    val covers = !LocalListPane.current && (if (display != null) display.layout == LibraryLayout.GRID else page.covers)
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
                    onOpen, onMenu, { _, _ -> }, browse::favorite, onPlaylist, { if (active) onScrolled(it) },
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
                onOpen, onMenu, { item, placement, shuffled -> page.play(placement, shuffled, item.row.title) }, browse::favorite,
                onPlaylist, { route, title -> browse.openRoute(route, title, summary.row.title) },
                onLink = { link -> model.runAction {
                    openMediaUri(context, link.url ?: page.sourceUri() ?: return@runAction)
                } }, onScrolled, onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) },
                onDownload = { browse.download(it, remove = it.row.downloaded) })
            return@Column
        }
        val select = LocalSelectMode.current
        if (select.route == location.route && location.route.let { it is Route.Playlists || it is Route.SmartPlaylists || it is Route.PlaylistFolder })
            PlaylistListSelectMode(model, browse, page, rows, player) { select.route = null }
        if (choices.isNotEmpty()) SegmentedPill(choices.map { it.category }, options.category,
            { options.category = it; options.sort = null; options.displaySettings = null },
            Modifier.padding(start = 12.dp, end = 12.dp, bottom = 4.dp), scroll = rememberScrollState()) { category, _ ->
            Text(choices.first { it.category == category }.title, style = MaterialTheme.typography.titleSmall, maxLines = 1)
        }
        Box(Modifier.weight(1f)) {
            BrowseContent(rows, page.homeSections, player, location.route is Route.Home, location in browse.details, covers,
                onOpen, onMenu, { section, shuffled -> page.playSection(section, shuffled) },
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
