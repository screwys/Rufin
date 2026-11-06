package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.library.FavoriteTarget
import io.github.screwys.rufin.shared.library.StateChange
import io.github.screwys.rufin.shared.rufin_core.CollectionCategory
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.rufin_core.routeRequiresSelectedSource
import io.github.screwys.rufin.shared.library.SearchMode
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.runtime.*
import androidx.compose.runtime.snapshots.Snapshot
import androidx.paging.*
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

/** A Home section: its title, rows, and the block its refresh reads again. */
internal data class HomeSectionItems(val index: ULong, val title: String, val kind: AndroidHomeSectionKind,
    val refresh: io.github.screwys.rufin.shared.library.HomeBlockKind?, val items: List<BrowseItem>)

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class BrowsePage(
    private val connection: RufinConnection,
    private val browse: BrowseConnection,
    val location: BrowseLocation,
    val options: BrowseOptions,
    scope: CoroutineScope,
    private val pins: Boolean = false,
    private val parent: BrowsePage? = null,
) {
    var active by mutableStateOf(false)
    var favoritesPage by mutableStateOf<BrowsePage?>(null)
        private set
    var releaseGroups by mutableStateOf<List<AndroidArtistReleaseGroup>>(emptyList())
        private set
    var supportsFavoriteFilter by mutableStateOf(false)
        private set
    var supportsDownloadedFilter by mutableStateOf(false)
        private set
    var homeSections by mutableStateOf<List<HomeSectionItems>>(emptyList())
        private set
    var scrollSections by mutableStateOf<List<AndroidScrollSection>>(emptyList())
        private set
    var sortFields by mutableStateOf<List<AndroidSortField>>(emptyList())
        private set
    val sortSelection get() = capturedDisplay?.settings
    private var capturedDisplay by mutableStateOf<AndroidBrowseDisplay?>(null)
    val displaySettings get() = if (artistAlbums) browse.artistAlbumsDisplay ?: capturedDisplay else capturedDisplay
    var breadcrumbs by mutableStateOf<List<AndroidBreadcrumb>>(emptyList())
        private set
    var musicFolders by mutableStateOf<AndroidMusicFolderState?>(null)
        private set
    private val artistAlbums get() = location.route.showsReleases && options.category == CollectionCategory.ALBUMS
    var summary by mutableStateOf<AndroidDetailSummary?>(null)
        private set
    /** The fields the page's rows show, in its layout. */
    var fields by mutableStateOf<List<AndroidSortField>>(emptyList())
        private set
    var loading by mutableStateOf(true)
        private set
    private var revision by mutableIntStateOf(0)
    /** Committed library changes this page's view said it shows. */
    private var changes by mutableIntStateOf(0)
    private var currentView by mutableStateOf<AndroidView?>(null)
    private var openedPage by mutableStateOf<AndroidOpened?>(null)
    private var preparedContext by mutableStateOf<PageContext?>(null)
    private var capturedLabelsVersion = browse.labelsVersion
    /** The favorites and ratings committed while the page is open; rows it read before show
     * them through [shown]. */
    private val states = mutableStateMapOf<FavoriteTarget, StateChange>()
    /** How many download changes were committed while the page is open; rows read before the
     * latest show the current downloads through [shown]. */
    private var downloads by mutableIntStateOf(0)
    private val data = MutableStateFlow<PagingData<BrowseItem>>(PagingData.empty())
    val pages: Flow<PagingData<BrowseItem>> = data.cachedIn(scope)

    val covers get() = displaySettings?.layout == LibraryLayout.GRID
    val hasActiveFilters get() = options.filter.isNotBlank() ||
        (supportsFavoriteFilter && options.favoritesOnly) || (supportsDownloadedFilter && options.downloadedOnly)
    fun resetFilters() {
        Snapshot.withMutableSnapshot {
            options.filter = ""
            options.favoritesOnly = false
            options.downloadedOnly = false
        }
    }
    private val embedded get() = parent != null
    var supportsLetterIndex by mutableStateOf(false)
        private set
    suspend fun letterPosition(letter: String): ULong? {
        val view = currentView ?: return null
        val sections = if (scrollSections.isEmpty()) view.sections() else scrollSections
        if (currentView !== view) return null
        scrollSections = sections
        return sections.firstOrNull { it.title == letter }?.index
            ?: sections.filter { it.title <= letter }.maxByOrNull { it.title }?.index
            ?: sections.firstOrNull()?.index
    }

    private data class PageContext(
        val library: AndroidLibrary?, val opens: Boolean, val changes: Int,
        val route: Route, val category: CollectionCategory, val historySource: String?, val filter: String,
        val sort: LibraryField?, val descending: Boolean, val favoritesOnly: Boolean, val downloadedOnly: Boolean,
        val revision: Int, val artistAlbumsRevision: Int,
        val homeBlocks: String?,
        val parentPage: AndroidOpened?,
    )

    /** `item` showing the favorite and rating committed for it since the page read it, and the
     * downloads as they are now. */
    @Composable
    fun shown(item: BrowseItem): BrowseItem {
        val change = item.row.target?.let { states[it] }
        val revision = downloads
        var current by remember(item.row) { mutableStateOf(item.row) }
        LaunchedEffect(item.row, revision) {
            if (item.downloads != revision) browse.library?.showDownloads(item.row, item.view)?.let { current = it }
        }
        if (change == null && current === item.row) return item
        return remember(current, change) {
            var row = current
            change?.let { showState(row, it) }?.let { row = it }
            if (row === item.row) item else item.copy(row = row)
        }
    }

    fun refresh() { loading = true; revision++ }
    suspend fun sourceUri(): String? = currentView?.folderUri()
    fun chooseSort(field: LibraryField, direction: Boolean) = connection.runAction {
        withContext(Dispatchers.IO) { browse.library?.setBrowseSort(location.route, options.category, field, direction) }
        if (artistAlbums) {
            options.sort = null
            browse.artistAlbumsChanged(orderChanged = true)
        } else {
            options.sort = field
            options.descending = direction
        }
    }
    fun chooseDisplay(layout: LibraryLayout, size: DisplaySize, spacing: DisplaySize) = connection.runAction {
        val previous = (options.displaySettings ?: displaySettings)?.layout
        val display = withContext(Dispatchers.IO) {
            browse.library?.setBrowseDisplay(location.route, options.category, layout, size, spacing)
        } ?: return@runAction
        showDisplay(display, previous != display.layout)
    }
    fun showField(field: LibraryField, shown: Boolean) = connection.runAction {
        val display = withContext(Dispatchers.IO) {
            browse.library?.showBrowseField(location.route, options.category, field, shown)
        } ?: return@runAction
        showDisplay(display, true)
    }
    fun moveField(field: LibraryField, up: Boolean) = connection.runAction {
        val display = withContext(Dispatchers.IO) {
            browse.library?.moveBrowseField(location.route, options.category, field, up)
        } ?: return@runAction
        showDisplay(display, true)
    }
    /** Shows `display`; rows read again when the fields they show changed. */
    private fun showDisplay(display: AndroidBrowseDisplay, fieldsChanged: Boolean) {
        if (artistAlbums) {
            options.displaySettings = null
            browse.artistAlbumsChanged(display, orderChanged = fieldsChanged)
        } else if (fieldsChanged) { options.displaySettings = null; refresh() }
        else options.displaySettings = display
    }

    fun play(placement: QueuePlacement, shuffled: Boolean, title: String) = connection.runAction {
        val view = currentView ?: return@runAction
        connection.connectedService().playRequest { view.playAll(placement, shuffled, title) }
    }

    fun playSection(section: HomeSectionItems, shuffled: Boolean = false) = connection.runAction {
        val view = currentView ?: return@runAction
        connection.connectedService().playRequest { view.playSection(section.index, 0UL, shuffled, section.title) }
    }

    private fun homeSections(view: AndroidView, sections: List<AndroidHomeSection>, display: AndroidBrowseDisplay?) =
        sections.mapIndexed { section, home ->
            HomeSectionItems(section.toULong(), home.title, home.kind, home.refresh, home.rows.mapIndexed { index, row ->
                BrowseItem(row, index.toULong(), view, display, section = section.toULong(), contextTitle = home.title,
                    downloads = downloads)
            })
        }

    @OptIn(ExperimentalCoroutinesApi::class)
    suspend fun observe(): Unit = withContext(Dispatchers.IO) { coroutineScope {
        val lifetime = coroutineContext
        fun sameContext(previous: PageContext?, current: PageContext?): Boolean =
            previous != null && previous == current
        launch {
            connection.changes.collect { changed ->
                changed.items.forEach { states[it.target] = it }
                if (changed.downloads.isNotEmpty()) {
                    downloads++
                    val read = summary
                    val row = read?.let { browse.library?.showDownloads(it.row, currentView) }
                    if (row != null && summary === read) summary = read.copy(row = row)
                }
                if (if (pins) browse.library?.pinsRefresh(changed) == true else currentView?.refresh(changed) == true) changes++
            }
        }
        var favoriteJob: Job? = null
        launch {
            snapshotFlow { Triple(parent?.active ?: active, browse.labelsVersion, currentView) }
                .collectLatest { (active, labelsVersion, view) ->
                    if (!active || view == null || labelsVersion == capturedLabelsVersion) return@collectLatest
                    try {
                    val sorts = view.sortFields()
                    val display = view.displaySettings()
                    val shownFields = view.fields()
                    val homeTitles = view.homeTitles()
                    if (currentView !== view) return@collectLatest
                    Snapshot.withMutableSnapshot {
                        capturedLabelsVersion = labelsVersion
                        sortFields = sorts
                        capturedDisplay = display
                        fields = shownFields
                        summary = openedPage?.summary?.let { detail ->
                            detail.copy(links = detail.links.map { it.copy(title = translate(it.title)) })
                        }
                        releaseGroups = openedPage?.groups.orEmpty().map { it.copy(title = translate(it.title)) }
                        homeSections = homeSections.mapIndexed { index, section ->
                            homeTitles.getOrNull(index)?.let { title -> section.copy(title = title) } ?: section
                        }
                    }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                }
        }
        snapshotFlow {
            // A page that lists a source opens once one is selected; selecting another source or
            // music folder reaches it through `changes`.
            val needsSource = !pins && routeRequiresSelectedSource(location.route, SearchMode.TITLE)
            if (!(parent?.active ?: active) || (parent != null && parent.preparedContext == null)) null else PageContext(browse.library,
                !needsSource || connection.sources?.selectedSourceId != null,
                if (parent != null) parent.preparedContext?.changes ?: 0 else changes,
                location.route, options.category, options.historySource, options.filter, options.sort, options.descending,
                options.favoritesOnly, options.downloadedOnly,
                revision + (parent?.revision ?: 0), if (artistAlbums) browse.artistAlbumsRevision else 0,
                if (location.route is Route.Home && !pins) browse.homeBlocksFingerprint else null, parent?.openedPage)
        }.distinctUntilChanged(::sameContext).mapLatest { context ->
            val library = context?.library ?: return@mapLatest null
            if (sameContext(preparedContext, context)) return@mapLatest null
            loading = true
            try {
                val labelsVersion = browse.labelsVersion
                val route = context.route
                if (!context.opens) return@mapLatest null
                // An artist's favorite tracks start from the rows the artist's page read.
                val favoriteSeed = context.parentPage?.takeIf {
                    context.filter == parent?.preparedContext?.filter && !context.favoritesOnly && !context.downloadedOnly
                }
                val opened = if (pins) null else library.open(route, context.category, context.filter, context.sort,
                    context.descending, context.favoritesOnly, context.downloadedOnly, context.historySource,
                    currentView, embedded)
                val view = opened?.view
                val count = favoriteSeed?.summary?.favoriteTotal?.toInt() ?: opened?.count?.toInt()
                val seedRows = if (context.sort == null) favoriteSeed?.favoriteRows ?: opened?.rows else opened?.rows
                val display = view?.displaySettings()
                val detail = opened?.summary
                val home = if (view != null && route is Route.Home) homeSections(view, opened.home, display) else emptyList()
                val favoriteRoute = when (route) {
                    is Route.ArtistDetail -> Route.ArtistFavoriteTracks(route.v1)
                    is Route.AlbumArtistDetail -> Route.AlbumArtistFavoriteTracks(route.v1)
                    else -> null
                }?.takeIf { detail != null }
                val favorite = if (favoritesPage?.location?.route == favoriteRoute) favoritesPage else {
                    favoriteJob?.cancel()
                    favoriteJob = null
                    favoriteRoute?.let { favorites ->
                        val childJob = Job(lifetime[Job])
                        val childScope = CoroutineScope(lifetime + childJob)
                        val childOptions = BrowseOptions().also { it.filter = context.filter }
                        val childLocation = BrowseLocation(favorites, "Favorite tracks", childOptions, location.playbackTitle)
                        BrowsePage(connection, browse, childLocation, childOptions, childScope, parent = this@BrowsePage).also { page ->
                            favoriteJob = childJob
                            childScope.launch { page.observe() }
                        }
                    }
                }
                val sorts = view?.sortFields().orEmpty()
                val shownFields = view?.fields().orEmpty()
                val crumbs = if (embedded) emptyList() else view?.breadcrumbs().orEmpty()
                val folders = if (route is Route.Folders) library.musicFolders() else null
                Snapshot.withMutableSnapshot {
                    capturedLabelsVersion = labelsVersion
                    currentView = view
                    openedPage = opened
                    preparedContext = if (home.isNotEmpty() || route is Route.Home) context else null
                    scrollSections = emptyList()
                    supportsLetterIndex = !embedded && view?.hasLetterSections() == true
                    supportsFavoriteFilter = view?.supportsFavoriteFilter() == true
                    supportsDownloadedFilter = view?.supportsDownloadedFilter() == true
                    sortFields = sorts
                    capturedDisplay = display
                    summary = detail
                    fields = shownFields
                    breadcrumbs = crumbs
                    musicFolders = folders
                    homeSections = home
                    favorite?.let { page ->
                        if (page.options.filter != context.filter) page.loading = true
                        page.options.filter = context.filter
                    }
                    favoritesPage = favorite
                    releaseGroups = opened?.groups.orEmpty()
                }
                if (route is Route.Home && !pins) return@mapLatest null
                context to Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
                    maxSize = 192, enablePlaceholders = !pins,
                    jumpThreshold = if (pins) PagingSource.LoadResult.Page.COUNT_UNDEFINED else 192)) {
                    BrowsePagingSource(library, view, count, pins, display, location.playbackTitle, seedRows) { downloads }
                }.flow
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { connection.runAction { throw error }; null }
            finally { loading = false }
        }.filterNotNull().collectLatest { (context, pages) ->
            preparedContext = context
            pages.collect { data.value = it }
        }
    } }
}

private class BrowsePagingSource(
    private val library: AndroidLibrary,
    private val view: AndroidView?,
    private val count: Int?,
    private val pins: Boolean,
    private val display: AndroidBrowseDisplay?,
    private val contextTitle: String,
    private val firstRows: List<AndroidBrowseRow>?,
    /** The page's download changes so far, which the rows it reads show. */
    private val downloads: () -> Int,
) : PagingSource<Int, BrowseItem>() {
    override val jumpingSupported = true
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, BrowseItem> = try {
        withContext(Dispatchers.Default) {
            val offset = params.key ?: 0
            val revision = downloads()
            val pinPage = if (pins) library.pins(offset.toULong(), params.loadSize.toUInt()) else null
            val total = pinPage?.total?.toInt() ?: count ?: 0
            val rows = pinPage?.rows ?: if (offset == 0 && firstRows != null) {
                val ready = firstRows.take(params.loadSize)
                if (ready.size >= params.loadSize || ready.size >= total) ready
                else ready + view!!.rows(ready.size.toULong(), (params.loadSize - ready.size).toUInt())
            } else view!!.rows(offset.toULong(), params.loadSize.toUInt())
            LoadResult.Page(rows.mapIndexed { index, row ->
                BrowseItem(row, (offset + index).toULong(), view, display, contextTitle = contextTitle, downloads = revision)
            },
                if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                if (pins) pinPage?.nextOffset?.toInt() else if (offset + rows.size >= total) null else offset + rows.size,
                itemsBefore = if (pins) LoadResult.Page.COUNT_UNDEFINED else offset,
                itemsAfter = if (pins) LoadResult.Page.COUNT_UNDEFINED else (total - offset - rows.size).coerceAtLeast(0))
        }
    } catch (cancelled: CancellationException) { throw cancelled }
    catch (error: Exception) { LoadResult.Error(error) }

    override fun getRefreshKey(state: PagingState<Int, BrowseItem>): Int? = state.anchorPosition?.let {
        (it - state.config.initialLoadSize / 2).coerceAtLeast(0)
    }
}

internal val AndroidBrowseDisplay.layout get() = settings.layout
internal val AndroidBrowseDisplay.size get() = settings.display.size
internal val AndroidBrowseDisplay.gridSpacing get() = settings.display.gridSpacing
internal val AndroidBrowseDisplay.showHeader get() = settings.display.showHeader
