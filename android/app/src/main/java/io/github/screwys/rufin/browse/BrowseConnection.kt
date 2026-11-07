package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.rufin_core.Refresh
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.rufin_core.SidebarRouteItem
import io.github.screwys.rufin.shared.library.SearchMode
import io.github.screwys.rufin.shared.library.FavoriteTarget
import io.github.screwys.rufin.shared.library.HomeBlockKind
import io.github.screwys.rufin.shared.rufin_core.CollectionCategory
import io.github.screwys.rufin.shared.rufin_core.RecentSearchKind
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.runtime.*
import androidx.compose.runtime.snapshots.Snapshot
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

internal enum class AppSection { Browse, Search, Pins, More }

internal class BrowseOptions {
    var category by mutableStateOf(CollectionCategory.TRACKS)
    /** The source whose plays History lists, the one selected when it was picked; every
     * source's when null. */
    var historySource by mutableStateOf<String?>(null)
    var filter by mutableStateOf("")
    var favoritesOnly by mutableStateOf(false)
    var downloadedOnly by mutableStateOf(false)
    var sort by mutableStateOf<io.github.screwys.rufin.shared.library.LibraryField?>(null)
    var descending by mutableStateOf(false)
    var displaySettings by mutableStateOf<AndroidBrowseDisplay?>(null)
}

internal data class BrowseLocation(
    val route: Route, val title: String,
    val options: BrowseOptions = BrowseOptions(),
    val playbackTitle: String = title,
)
/** A row and where it shows: its place in `view`, or in Home's `section`, or among search results.
 *  `downloads` is how many download changes the page had when it read it. */
internal data class BrowseItem(val row: AndroidBrowseRow, val index: ULong, val view: AndroidView?,
    val displaySettings: AndroidBrowseDisplay? = null, val section: ULong? = null,
    val searchResult: Boolean = false, val contextTitle: String? = null,
    val downloads: Int = 0)

@OptIn(FlowPreview::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class BrowseConnection(private val connection: RufinConnection, private val scope: CoroutineScope) {
    var section by mutableStateOf(AppSection.Browse)
    var routes by mutableStateOf<List<AndroidRouteDescriptor>>(emptyList())
        private set
    var configuredRoutes by mutableStateOf<List<AndroidRouteDescriptor>>(emptyList())
        private set
    var routeSettings by mutableStateOf<List<AndroidRouteSetting>>(emptyList())
        private set
    var categories by mutableStateOf<List<AndroidCollectionCategory>>(emptyList())
        private set
    var rootId by mutableStateOf("Home")
    var details by mutableStateOf<List<BrowseLocation>>(emptyList())
        private set
    var searchText by mutableStateOf("")
    var searchMode by mutableStateOf(SearchMode.TITLE)
    private var searchSubmission by mutableIntStateOf(0)
    private var searchChanges by mutableIntStateOf(0)
    private var searchRereads by mutableIntStateOf(0)
    /** The Search page that found `searchRows`; it says when a change means searching again. */
    @Volatile private var searchView: AndroidView? = null
    var searchRows by mutableStateOf<List<BrowseItem>>(emptyList())
        private set
    var recentSearchRows by mutableStateOf<List<BrowseItem>>(emptyList())
        private set
    var library by mutableStateOf<AndroidLibrary?>(null)
        private set
    var visiblePage by mutableStateOf<BrowsePage?>(null)
    var labelsVersion by mutableIntStateOf(0)
        private set
    var homeBlocksFingerprint by mutableStateOf<String?>(null)
    var artistAlbumsRevision by mutableIntStateOf(0)
        private set
    var artistAlbumsDisplay by mutableStateOf<AndroidBrowseDisplay?>(null)
        private set
    fun artistAlbumsChanged(display: AndroidBrowseDisplay? = null, orderChanged: Boolean = false) {
        artistAlbumsDisplay = display
        if (orderChanged) artistAlbumsRevision++
    }
    private var startupRouteSelected = false
    private var translationVersion: Int? = null
    private var searchLoading by mutableStateOf(false)
    private val routeOptions = mutableMapOf<String, BrowseOptions>()

    fun optionsFor(rootId: String): BrowseOptions = routeOptions.getOrPut(rootId) { BrowseOptions() }
    private val currentOptions get() = details.lastOrNull()?.options ?: optionsFor(rootId)
    var category: CollectionCategory
        get() = currentOptions.category
        set(value) { currentOptions.category = value }
    var filter: String
        get() = currentOptions.filter
        set(value) { currentOptions.filter = value }
    val scrollSections get() = visiblePage?.scrollSections.orEmpty()
    val loading get() = if (section == AppSection.Search) searchLoading else visiblePage?.loading == true
    val detail get() = details.isNotEmpty()
    val collectionCategories get() = when {
        !detail && rootId == "Favorites" -> categories
        details.lastOrNull()?.route?.showsReleases == true -> categories.filter { it.category != CollectionCategory.ARTISTS }
        else -> emptyList()
    }
    val current get() = details.lastOrNull() ?: routes.firstOrNull { it.id == rootId }
        ?.let { BrowseLocation(it.route, it.title, optionsFor(it.id)) }

    private data class SearchContext(val section: AppSection, val text: String, val mode: SearchMode,
        val library: AndroidLibrary?, val changes: Int, val submission: Int, val rereads: Int)

    fun submitSearch() {
        if (searchMode == SearchMode.LYRICS) searchSubmission++
    }

    init {
        scope.launch {
            snapshotFlow { connection.ready to connection.service }.collectLatest { (ready, service) ->
                library = null
                if (!ready || service == null) return@collectLatest
                var bridge: AndroidLibrary? = null
                try {
                    val runtime = service.runtime.value?.getOrNull() ?: return@collectLatest
                    val current = runtime.library()
                    bridge = current
                    val startingPage = if (!startupRouteSelected) withContext(Dispatchers.IO) {
                        val settings = runtime.settings()
                        try { settings.applicationPreferences().startingPage } finally { settings.destroy() }
                    } else null
                    loadDescriptors(current, startingPage)
                    startupRouteSelected = true
                    library = current
                    recentSearchRows = loadRecentSearches(current)
                    awaitCancellation()
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
                finally { library = null; bridge?.destroy() }
            }
        }
        scope.launch(Dispatchers.Main) {
            connection.changes.collect { changed ->
                when (searchView?.refresh(changed)) {
                    Refresh.REOPEN -> searchChanges++
                    Refresh.REREAD -> searchRereads++
                    else -> {}
                }
            }
        }
        scope.launch {
            var submitted = 0
            var searched: SearchContext? = null
            snapshotFlow { SearchContext(section, searchText, searchMode, library, searchChanges, searchSubmission, searchRereads) }
                .debounce { if (it.submission != submitted) 0L else 250L }.collectLatest { context ->
                    val explicit = context.submission != submitted
                    submitted = context.submission
                    // Only a reread asked: the Search page reads its results again.
                    val again = searchView.takeIf { searched?.copy(rereads = context.rereads) == context }
                    searched = context
                    val query = context.text.trim()
                    if (context.section != AppSection.Search || query.isEmpty() || context.library == null ||
                        context.mode == SearchMode.LYRICS && !explicit && query.codePointCount(0, query.length) < 2) {
                        searchRows = emptyList()
                        searchView = null
                        searchLoading = false
                        return@collectLatest
                    }
                    searchLoading = true
                    try {
                        val found = withContext(Dispatchers.Default) {
                            if (again != null) context.library.searchAgain(again) else context.library.search(context.text, context.mode)
                        }
                        // What committed while it searched reaches the results as any other change;
                        // results of a library no longer selected are never shown.
                        val missed = found.reading.missed()
                        val refresh = missed?.let { found.view.refresh(it) } ?: Refresh.NOTHING
                        if (refresh == Refresh.REOPEN) {
                            searchChanges++
                            return@collectLatest
                        }
                        val rows = found.rows.mapIndexed { index, row ->
                            BrowseItem(row, index.toULong(), found.view, searchResult = true, contextTitle = context.text)
                        }
                        searchView = found.view
                        searchRows = rows
                        // Results show changes through the visible page, as its rows do.
                        visiblePage?.let { page ->
                            page.hold(rows)
                            missed?.let(page::showChanges)
                        }
                        if (refresh == Refresh.REREAD) searchRereads++
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                    finally { searchLoading = false }
                }
        }
    }

    private suspend fun loadDescriptors(bridge: AndroidLibrary, startingPage: SidebarRouteItem? = null) = withContext(Dispatchers.IO) {
        val all = bridge.routes()
        val configured = bridge.configuredRoutes()
        val settings = bridge.routeSettings()
        val collections = bridge.collectionCategories()
        Snapshot.withMutableSnapshot {
            routes = all
            configuredRoutes = configured
            routeSettings = settings
            categories = collections
            if (all.none { it.id == rootId }) rootId = configured.firstOrNull { it.id != "Search" }?.id ?: "Home"
            all.firstOrNull { it.item == startingPage }?.let { route ->
                if (route.id == "Search") section = AppSection.Search else chooseRoot(route.id)
            }
        }
    }

    fun setRouteVisible(id: String, visible: Boolean) = connection.runAction {
        val bridge = library ?: return@runAction
        if (bridge.setRouteVisible(id, visible)) loadDescriptors(bridge)
    }

    fun moveRoute(id: String, up: Boolean) = connection.runAction {
        val bridge = library ?: return@runAction
        if (bridge.moveRoute(id, up)) loadDescriptors(bridge)
    }

    fun reorderRoute(id: String, targetId: String, after: Boolean) = connection.runAction {
        val bridge = library ?: return@runAction
        bridge.reorderRoute(id, targetId, after)
        loadDescriptors(bridge)
    }

    fun chooseRoot(id: String) { rootId = id; details = emptyList() }
    fun back() { details = details.dropLast(1) }
    fun folderAncestor(route: Route, title: String) {
        val index = details.indexOfLast { it.route == route }
        if (index >= 0) details = details.take(index + 1)
        else if (routes.any { it.id == "Folders" && it.route == route }) chooseRoot("Folders")
        else details = details.dropLast(1) + BrowseLocation(route, title, currentOptions)
    }
    fun openRoute(route: Route, title: String, playbackTitle: String = title) {
        section = AppSection.Browse
        val options = BrowseOptions().also { it.category = if (route.showsReleases) CollectionCategory.ALBUMS else CollectionCategory.TRACKS }
        details = details + BrowseLocation(route, title, options, playbackTitle)
    }
    fun openLinkedRoute(route: Route, title: String, sourceId: String?) {
        if (sourceId == null || sourceId == connection.sources?.selectedSourceId) {
            openRoute(route, title)
        } else connection.runAction {
            connection.sourceOwner?.selectSource(sourceId)
            openRoute(route, title)
        }
    }
    fun open(item: BrowseItem) {
        val route = item.row.route ?: return play(item)
        connection.runAction {
            item.row.sourceId?.takeIf { it != connection.sources?.selectedSourceId }?.let { source -> connection.sourceOwner?.selectSource(source) }
            section = AppSection.Browse
            val options = BrowseOptions().also {
                it.category = if (route.showsReleases) CollectionCategory.ALBUMS else CollectionCategory.TRACKS
            }
            details = details + BrowseLocation(route, item.row.title, options)
            rememberSearchResult(item)
        }
    }

    private suspend fun loadRecentSearches(bridge: AndroidLibrary): List<BrowseItem> =
        bridge.recentSearchResults().mapIndexed { index, row ->
            BrowseItem(row, index.toULong(), null, searchResult = true, contextTitle = translate("Recent searches"))
        }

    private fun rememberSearchResult(item: BrowseItem) {
        if (!item.searchResult) return
        val bridge = library ?: return
        connection.runAction {
            val kind = when (item.row.target) {
                is FavoriteTarget.Track -> RecentSearchKind.TRACK
                is FavoriteTarget.Album -> RecentSearchKind.ALBUM
                is FavoriteTarget.Artist -> RecentSearchKind.ARTIST
                null -> return@runAction
            }
            if (withContext(Dispatchers.IO) { bridge.rememberSearchResult(io.github.screwys.rufin.shared.rufin_core.RecentSearchResult(
                kind, item.row.mediaUri, item.row.title, item.row.subtitle, item.row.artworkIdentity)) }) {
                recentSearchRows = loadRecentSearches(bridge)
            }
        }
    }

    fun removeRecentSearch(item: BrowseItem) = connection.runAction {
        val bridge = library ?: return@runAction
        val recent = item.row.recent ?: return@runAction
        if (withContext(Dispatchers.IO) { bridge.removeSearchResult(recent.kind, recent.mediaUri) }) {
            recentSearchRows = loadRecentSearches(bridge)
        }
    }

    fun clearRecentSearches() = connection.runAction {
        val bridge = library ?: return@runAction
        if (withContext(Dispatchers.IO) { bridge.clearSearchResults() }) recentSearchRows = emptyList()
    }
    fun play(item: BrowseItem, placement: QueuePlacement = QueuePlacement.Replace(0UL), shuffled: Boolean = false) = connection.runAction {
        val library = library ?: return@runAction
        // Core decides what the row plays from where it shows.
        connection.connectedService().playRequest {
            library.play(item.row, item.view, item.index, placement, shuffled, item.contextTitle)
        }
        rememberSearchResult(item)
    }
    fun favorite(item: BrowseItem) {
        val value = !item.row.favorite
        val target = item.row.target ?: return
        connection.runAction { connection.sourceOwner?.setFavoriteWithResult(target, value) }
    }
    fun radio(item: BrowseItem, placement: QueuePlacement) = connection.runAction {
        connection.connectedService().playRequest {
            library?.playRadio((item.row.target as? FavoriteTarget.Track)?.v1, item.row.route, placement)
        }
    }
    fun pin(item: BrowseItem, pinned: Boolean) = connection.runAction {
        withContext(Dispatchers.IO) { item.row.pin?.let { library?.setPin(it, pinned) } }
        refresh()
    }
    fun refresh() { visiblePage?.refresh() }
    fun download(item: BrowseItem, remove: Boolean = false) = connection.runAction {
        library?.downloadTarget(item.row.route, item.row.mediaUri, remove)
    }
    fun refreshLabels(version: Int) {
        val previous = translationVersion
        translationVersion = version
        if (previous == null || previous == version) return
        val bridge = library ?: return
        connection.runAction {
            loadDescriptors(bridge)
            artistAlbumsDisplay = null
            labelsVersion++
        }
    }
    fun refreshSection(block: HomeBlockKind) = connection.runAction { library?.refreshHome(block) }
}
