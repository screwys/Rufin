package io.github.screwys.rufin.browse

import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.runtime.*
import androidx.compose.runtime.snapshots.Snapshot
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

internal enum class AppSection { Browse, Search, Pins, More }

internal class BrowseOptions {
    var preparing by mutableStateOf(false)
    var category by mutableStateOf("tracks")
    var filter by mutableStateOf("")
    var favoritesOnly by mutableStateOf(false)
    var downloadedOnly by mutableStateOf(false)
    var sort by mutableStateOf("")
    var descending by mutableStateOf(false)
    var displaySettings by mutableStateOf<AndroidBrowseDisplay?>(null)
}

internal data class BrowseLocation(
    val route: String, val title: String, val kind: String = "",
    val options: BrowseOptions = BrowseOptions(),
    val playbackTitle: String = title,
)
internal data class BrowseItem(val row: AndroidBrowseRow, val index: ULong, val query: AndroidBrowseQuery?,
    val displaySettings: AndroidBrowseDisplay? = null, val fromPins: Boolean = false,
    val searchOrder: List<String>? = null, val searchContext: String? = null, val contextTitle: String? = null)

@OptIn(FlowPreview::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class BrowseConnection(private val connection: RufinConnection, scope: CoroutineScope) {
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
    var showDownloadedBadges by mutableStateOf(true)
    var artistAlbumsRevision by mutableIntStateOf(0)
        private set
    var artistAlbumsDisplay by mutableStateOf<AndroidBrowseDisplay?>(null)
        private set
    fun artistAlbumsChanged(display: AndroidBrowseDisplay? = null, orderChanged: Boolean = false) {
        artistAlbumsDisplay = display
        if (orderChanged) artistAlbumsRevision++
    }
    private var translationVersion: Int? = null
    private var searchLoading by mutableStateOf(false)
    private val routeOptions = mutableMapOf<String, BrowseOptions>()

    fun optionsFor(rootId: String): BrowseOptions = routeOptions.getOrPut(rootId) { BrowseOptions() }
    private val currentOptions get() = details.lastOrNull()?.options ?: optionsFor(rootId)
    var category: String
        get() = currentOptions.category
        set(value) { currentOptions.category = value }
    var filter: String
        get() = currentOptions.filter
        set(value) { currentOptions.filter = value }
    var sort: String
        get() = currentOptions.sort
        set(value) { currentOptions.sort = value }
    var descending: Boolean
        get() = currentOptions.descending
        set(value) { currentOptions.descending = value }
    val sortFields get() = visiblePage?.sortFields.orEmpty()
    val sortSelection get() = visiblePage?.sortSelection
    val displaySettings get() = currentOptions.displaySettings ?: visiblePage?.displaySettings
    val scrollSections get() = visiblePage?.scrollSections.orEmpty()
    val homeRows get() = visiblePage?.homeRows.orEmpty()
    val loading get() = if (section == AppSection.Search) searchLoading else visiblePage?.loading == true
    val detail get() = details.isNotEmpty()
    val collectionCategories get() = when {
        !detail && rootId == "Favorites" -> categories
        details.lastOrNull()?.kind == "artist" -> categories.filter { it.id != "artists" }
        else -> emptyList()
    }
    val current get() = details.lastOrNull() ?: routes.firstOrNull { it.id == rootId }
        ?.let { BrowseLocation(it.route, it.title, it.id, optionsFor(it.id)) }

    private data class SearchContext(val section: AppSection, val text: String,
        val library: AndroidLibrary?, val source: String?, val revision: ULong?)

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
                    loadDescriptors(current)
                    library = current
                    recentSearchRows = loadRecentSearches(current)
                    awaitCancellation()
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
                finally { library = null; bridge?.destroy() }
            }
        }
        scope.launch {
            snapshotFlow { SearchContext(section, searchText, library,
                connection.libraryState?.sourceId, connection.libraryState?.revision) }
                .debounce(250).collectLatest { context ->
                    if (context.section != AppSection.Search || context.text.isBlank() || context.library == null) {
                        searchRows = emptyList()
                        searchLoading = false
                        return@collectLatest
                    }
                    searchLoading = true
                    try {
                        val rows = context.library.search(context.text)
                        val order = rows.filter { it.kind == "track" }.map { it.mediaUri }
                        val capture = "search:${context.source}:${context.text}"
                        searchRows = rows.mapIndexed { index, row ->
                            BrowseItem(row, index.toULong(), null, searchOrder = order, searchContext = capture, contextTitle = context.text)
                        }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                    finally { searchLoading = false }
                }
        }
    }

    private suspend fun loadDescriptors(bridge: AndroidLibrary) = withContext(Dispatchers.IO) {
        val all = bridge.routes()
        val configured = bridge.configuredRoutes()
        val settings = bridge.routeSettings()
        val collections = bridge.collectionCategories()
        Snapshot.withMutableSnapshot {
            routes = all
            configuredRoutes = configured
            routeSettings = settings
            categories = collections
            if (configured.none { it.id == rootId }) rootId = configured.firstOrNull { it.id != "Search" }?.id ?: "Home"
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
    fun folderAncestor(route: String, title: String) {
        val index = details.indexOfLast { it.route == route }
        if (index >= 0) details = details.take(index + 1)
        else if (routes.any { it.id == "Folders" && it.route == route }) chooseRoot("Folders")
        else details = details.dropLast(1) + BrowseLocation(route, title, "folder", currentOptions)
    }
    fun openRoute(route: String, title: String, kind: String = "", playbackTitle: String = title) {
        section = AppSection.Browse
        val options = BrowseOptions().also { it.category = if (kind in listOf("artist", "ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography")) "albums" else "tracks" }
        details = details + BrowseLocation(route, title, kind, options, playbackTitle)
    }
    fun openLinkedRoute(route: String, title: String, sourceId: String?) {
        if (sourceId == null || sourceId == connection.sources?.selectedSourceId) {
            openRoute(route, title, library?.routeKind(route).orEmpty())
        } else connection.runAction {
            library?.selectSource(sourceId)
            openRoute(route, title, library?.routeKind(route).orEmpty())
        }
    }
    fun open(item: BrowseItem) {
        val route = item.row.detailRoute ?: return play(item)
        connection.runAction {
            item.row.sourceId?.takeIf { it != connection.sources?.selectedSourceId }?.let { source -> library?.selectSource(source) }
            section = AppSection.Browse
            val options = BrowseOptions().also {
                it.category = if (item.row.kind == "artist") "albums" else "tracks"
                it.preparing = item.searchContext != null && item.row.kind in setOf("album", "artist")
            }
            details = details + BrowseLocation(route, item.row.title, item.row.kind, options)
            rememberSearchResult(item)
            if (options.preparing) {
                try { library?.prepareCollection(item.row.mediaUri) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
                finally { options.preparing = false }
            }
        }
    }

    private suspend fun loadRecentSearches(bridge: AndroidLibrary): List<BrowseItem> = withContext(Dispatchers.IO) {
        bridge.recentSearchResults().mapIndexed { index, row ->
            BrowseItem(row, index.toULong(), null, searchContext = "recent:${row.kind}:${row.mediaUri}", contextTitle = translate("Recent searches"))
        }
    }

    private fun rememberSearchResult(item: BrowseItem) {
        if (item.searchContext == null) return
        val bridge = library ?: return
        connection.runAction {
            if (withContext(Dispatchers.IO) { bridge.rememberSearchResult(item.row) }) {
                recentSearchRows = loadRecentSearches(bridge)
            }
        }
    }

    fun removeRecentSearch(item: BrowseItem) = connection.runAction {
        val bridge = library ?: return@runAction
        if (withContext(Dispatchers.IO) { bridge.removeSearchResult(item.row.kind, item.row.mediaUri) }) {
            recentSearchRows = loadRecentSearches(bridge)
        }
    }

    fun clearRecentSearches() = connection.runAction {
        val bridge = library ?: return@runAction
        if (withContext(Dispatchers.IO) { bridge.clearSearchResults() }) recentSearchRows = emptyList()
    }
    fun play(item: BrowseItem, placement: String = "now", shuffled: Boolean = false) = connection.runAction {
        val service = connection.connectedService()
        service.playRequest {
        if (item.row.kind != "track") {
            item.row.detailRoute?.let { library?.playTarget(it, placement, shuffled, item.row.title) }
            return@playRequest
        }
        if (item.fromPins) {
            library?.playRows("pin:${item.row.key}", listOf(item.row.mediaUri), 0UL, placement, shuffled, translate("Pins"))
            return@playRequest
        }
        if (item.query != null) item.query.play(item.index, item.row.mediaUri, placement, shuffled, item.contextTitle)
        else {
            val order = item.searchOrder ?: listOf(item.row.mediaUri)
            library?.playRows(item.searchContext ?: "track:${item.row.mediaUri}", order,
                order.indexOf(item.row.mediaUri).toULong(), placement, shuffled, item.contextTitle ?: item.row.title)
        }
        }
        rememberSearchResult(item)
    }
    fun favorite(item: BrowseItem) {
        val value = !connection.projectedFavorite(item.row.kind, item.row.mediaUri, item.row.favorite)
        connection.setFavoriteIntent(item.row.kind, item.row.mediaUri, value)
        connection.runAction {
            try { library?.favorite(item.row.kind, item.row.mediaUri, value) }
            finally {
                connection.releaseFavoriteIntent(item.row.kind, item.row.mediaUri, value)
            }
        }
    }
    fun radio(item: BrowseItem, placement: String) = connection.runAction {
        connection.connectedService().playRequest {
            library?.playRadio(item.row.kind, item.row.mediaUri, item.row.detailRoute, placement)
        }
    }
    fun pin(item: BrowseItem, pinned: Boolean) = connection.runAction {
        withContext(Dispatchers.IO) { item.row.pin?.let { library?.setPin(it, pinned) } }
        refresh()
    }
    fun refresh() { visiblePage?.refresh() }
    fun download(item: BrowseItem, remove: Boolean = false) = connection.runAction {
        library?.downloadTarget(item.row.detailRoute, item.row.mediaUri, remove)
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
    fun playSection(id: String, shuffled: Boolean = false) = connection.runAction {
        val query = visiblePage?.homeRows?.firstOrNull { it.row.sectionId == id }?.query ?: return@runAction
        connection.connectedService().playRequest { query.playSection(id, "now", shuffled,
            visiblePage?.homeRows?.firstOrNull { it.row.sectionId == id }?.row?.section) }
    }
    fun refreshSection(id: String) = connection.runAction {
        library?.refreshHome(id)
        refresh()
    }
}
