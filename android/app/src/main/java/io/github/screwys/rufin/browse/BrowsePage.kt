package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.shared.library.LibraryField
import io.github.screwys.rufin.shared.rufin_core.Changed
import io.github.screwys.rufin.shared.rufin_core.CollectionCategory
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize
import io.github.screwys.rufin.shared.rufin_core.Refresh
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
    val items: List<BrowseItem>)

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class BrowsePage(
    private val connection: RufinConnection,
    private val browse: BrowseConnection,
    val location: BrowseLocation,
    val options: BrowseOptions,
    scope: CoroutineScope,
    private val pins: Boolean = false,
) : ShownRows(browse, scope) {
    var active by mutableStateOf(false)
    /** An artist page's favorite tracks: how many, the fields and display they show. */
    var favoriteCount by mutableStateOf(0UL)
        private set
    var favoriteFields by mutableStateOf<List<AndroidSortField>>(emptyList())
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
    override val header get() = summary?.row
    override val pageView get() = currentView
    override fun showHeader(row: AndroidBrowseRow) { summary = summary?.copy(row = row) }
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
    /** The favorite, rating and download changes committed while the page was not shown, which its
     * rows show once it is. */
    private var unshown: Changed? = null
    private val data = MutableStateFlow<PagingData<BrowseItem>>(PagingData.empty())
    val pages: Flow<PagingData<BrowseItem>> = data.cachedIn(scope)
    private val favoriteData = MutableStateFlow<PagingData<BrowseItem>>(PagingData.empty())
    val favoritePages: Flow<PagingData<BrowseItem>> = favoriteData.cachedIn(scope)

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
    )

    /** Handles a committed change: shows it in the rows and header, and reads the page again
     * when its View says so. Runs on the main thread. */
    private fun handle(changed: Changed) {
        if (changed.items.isNotEmpty() || changed.downloads.isNotEmpty()) {
            if (active) showChanges(changed)
            else unshown = unshown?.let { it.copy(items = it.items + changed.items, downloads = it.downloads.ifEmpty { changed.downloads }) } ?: changed
        }
        val refresh = if (pins) browse.library?.pinsRefresh(changed) == true
            else currentView?.refresh(changed)?.let { it != Refresh.NOTHING } == true
        if (refresh) changes++
    }

    fun refresh() { loading = true; revision++ }
    suspend fun sourceUri(): String? = currentView?.folderUri()
    fun chooseSort(field: LibraryField, direction: Boolean) = connection.runAction {
        withContext(Dispatchers.IO) { browse.library?.setBrowseSort(location.route, options.category, field, direction) }
        // An artist's releases read their saved order again as the settings commit.
        if (artistAlbums) options.sort = null
        else {
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
    fun reorderField(field: LibraryField, target: LibraryField, after: Boolean) = connection.runAction {
        val display = withContext(Dispatchers.IO) {
            browse.library?.reorderBrowseField(location.route, options.category, field, target, after)
        } ?: return@runAction
        showDisplay(display, true)
    }
    /** Shows `display`; rows read again when the fields they show changed. */
    private fun showDisplay(display: AndroidBrowseDisplay, fieldsChanged: Boolean) {
        if (artistAlbums) {
            options.displaySettings = null
            browse.artistAlbumsChanged(display, fieldsChanged)
        } else if (fieldsChanged) { options.displaySettings = null; refresh() }
        else options.displaySettings = display
    }

    fun play(placement: QueuePlacement, shuffled: Boolean, title: String) = connection.runAction {
        val view = currentView ?: return@runAction
        connection.connectedService().playRequest { view.playAll(placement, shuffled, title) }
    }

    fun playSection(section: HomeSectionItems, shuffled: Boolean = false) = connection.runAction {
        val view = currentView ?: return@runAction
        connection.connectedService().playRequest { view.playSection(section.index, shuffled, section.title) }
    }

    private fun homeSections(view: AndroidView, sections: List<AndroidHomeSection>, display: AndroidBrowseDisplay?,
        revision: Int) =
        sections.mapIndexed { section, home ->
            HomeSectionItems(section.toULong(), home.title, home.kind, home.rows.mapIndexed { index, row ->
                BrowseItem(row, index.toULong(), view, display, contextTitle = home.title,
                    downloads = revision)
            })
        }

    @OptIn(ExperimentalCoroutinesApi::class)
    suspend fun observe(): Unit = withContext(Dispatchers.IO) { coroutineScope {
        fun sameContext(previous: PageContext?, current: PageContext?): Boolean =
            previous != null && previous == current
        launch {
            connection.changes.collect { changed -> withContext(Dispatchers.Main) { handle(changed) } }
        }
        launch(Dispatchers.Main) {
            snapshotFlow { active }.collect { shown ->
                if (shown) unshown?.let { unshown = null; showChanges(it) }
            }
        }
        launch {
            // Committed settings read the page again as its View says; the Pins page, when its pins changed.
            snapshotFlow { connection.preferences?.let { if (pins) it.pins else it } }.filterNotNull()
                .distinctUntilChanged().drop(if (pins) 1 else 0).collect {
                    withContext(Dispatchers.Main) { if (pins || currentView?.settingsReread() == true) refresh() }
                }
        }
        launch {
            snapshotFlow { Triple(active, browse.labelsVersion, currentView) }
                .collectLatest { (active, labelsVersion, view) ->
                    if (!active || view == null || labelsVersion == capturedLabelsVersion) return@collectLatest
                    try {
                    val sorts = view.sortFields()
                    val display = view.displaySettings()
                    val shownFields = view.fields()
                    val favoriteShown = view.favoriteFields()
                    val homeTitles = view.homeTitles()
                    if (currentView !== view) return@collectLatest
                    Snapshot.withMutableSnapshot {
                        capturedLabelsVersion = labelsVersion
                        sortFields = sorts
                        capturedDisplay = display
                        fields = shownFields
                        favoriteFields = favoriteShown
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
            // A page that lists a source opens once one is selected and core has installed it, as
            // GTK waits; selecting another music folder reaches it through `changes`.
            val needsSource = !pins && routeRequiresSelectedSource(location.route, SearchMode.TITLE)
            if (!active) null else PageContext(browse.library,
                !needsSource || connection.sources?.selectedSourceId != null && !connection.sourceOperation.blocksLibrary(),
                changes,
                location.route, options.category, options.historySource, options.filter, options.sort, options.descending,
                options.favoritesOnly, options.downloadedOnly,
                revision, if (artistAlbums) browse.artistAlbumsRevision else 0)
        }.distinctUntilChanged(::sameContext).mapLatest { context ->
            val library = context?.library ?: return@mapLatest null
            if (sameContext(preparedContext, context)) return@mapLatest null
            loading = true
            try {
                val labelsVersion = browse.labelsVersion
                val route = context.route
                if (!context.opens) return@mapLatest null
                // The rows it reads show the changes heard so far; later ones reach them as patches.
                val opening = downloads
                val opened = if (pins) null else library.open(route, context.category, context.filter, context.sort,
                    context.descending, context.favoritesOnly, context.downloadedOnly, context.historySource,
                    currentView, false)
                val view = opened?.view
                val display = view?.displaySettings()
                val favoriteDisplay = view?.favoriteDisplay()
                fun seeds(rows: List<AndroidBrowseRow>?, display: AndroidBrowseDisplay?, favorite: Boolean = false) =
                    rows?.mapIndexed { index, row ->
                        BrowseItem(row, index.toULong(), view, display, contextTitle = location.playbackTitle,
                            downloads = opening, favorite = favorite)
                    }
                val seeds = seeds(opened?.rows, display)
                val favoriteSeeds = seeds(opened?.favoriteRows, favoriteDisplay, favorite = true)
                val detail = opened?.summary ?: summary.takeIf { opened?.keepsSummary == true }
                val home = if (view != null && route is Route.Home) homeSections(view, opened.home, display, opening) else emptyList()
                val sorts = view?.sortFields().orEmpty()
                val shownFields = view?.fields().orEmpty()
                val favoriteShown = view?.favoriteFields().orEmpty()
                val crumbs = view?.breadcrumbs().orEmpty()
                val folders = if (route is Route.Folders) library.musicFolders() else null
                withContext(Dispatchers.Main) {
                    Snapshot.withMutableSnapshot {
                        capturedLabelsVersion = labelsVersion
                        currentView = view
                        openedPage = opened?.copy(summary = detail)
                        preparedContext = if (home.isNotEmpty() || route is Route.Home) context else null
                        scrollSections = emptyList()
                        supportsLetterIndex = view?.hasLetterSections() == true
                        supportsFavoriteFilter = view?.supportsFavoriteFilter() == true
                        supportsDownloadedFilter = view?.supportsDownloadedFilter() == true
                        sortFields = sorts
                        capturedDisplay = display
                        summary = detail
                        fields = shownFields
                        favoriteFields = favoriteShown
                        favoriteCount = opened?.favoriteCount ?: 0UL
                        breadcrumbs = crumbs
                        musicFolders = folders
                        homeSections = home
                        releaseGroups = opened?.groups.orEmpty()
                    }
                    // What committed while it read reaches what it shows as any other change.
                    hold(seeds.orEmpty() + favoriteSeeds.orEmpty() + home.flatMap { it.items })
                    opened?.reading?.missed()?.let(::handle)
                }
                if (route is Route.Home && !pins) return@mapLatest null
                fun pager(count: Int?, display: AndroidBrowseDisplay?, seeds: List<BrowseItem>?, favorites: Boolean = false) =
                    Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
                        maxSize = 192, enablePlaceholders = !pins,
                        jumpThreshold = if (pins) PagingSource.LoadResult.Page.COUNT_UNDEFINED else 192)) {
                        BrowsePagingSource(library, view, count, pins, favorites, display, location.playbackTitle, seeds, ::hold,
                            { missed -> withContext(Dispatchers.Main) { handle(missed) } }) { downloads }
                    }.flow
                Triple(context, pager(opened?.count?.toInt(), display, seeds),
                    opened?.takeIf { it.favoriteCount > 0UL }?.let { pager(it.favoriteCount.toInt(), favoriteDisplay, favoriteSeeds, true) })
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { connection.runAction { throw error }; null }
            finally { loading = false }
        }.filterNotNull().collectLatest { (context, pages, favorites) ->
            preparedContext = context
            if (favorites == null) favoriteData.value = PagingData.empty()
            coroutineScope {
                favorites?.let { launch { it.collect { favoriteData.value = it } } }
                pages.collect { data.value = it }
            }
        }
    } }
}

private class BrowsePagingSource(
    private val library: AndroidLibrary,
    private val view: AndroidView?,
    private val count: Int?,
    private val pins: Boolean,
    /** Whether it lists an artist page's favorite tracks. */
    private val favorites: Boolean,
    private val display: AndroidBrowseDisplay?,
    private val contextTitle: String,
    /** The first rows, which the page read as it opened. */
    private val seeds: List<BrowseItem>?,
    /** Holds the rows it reads on the page, which patches them. */
    private val hold: (List<BrowseItem>) -> Unit,
    /** Handles what committed while it read, once it holds the rows, as the page does any change. */
    private val missed: suspend (Changed) -> Unit,
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
            val item = { index: Int, row: AndroidBrowseRow ->
                BrowseItem(row, (offset + index).toULong(), view, display, contextTitle = contextTitle, downloads = revision,
                    favorite = favorites)
            }
            var reading: AndroidReading? = null
            suspend fun window(from: Int, size: Int) = view!!.rows(from.toULong(), size.toUInt(), favorites).also { reading = it.reading }.rows
            val items = pinPage?.rows?.mapIndexed(item) ?: if (offset == 0 && seeds != null) {
                val ready = seeds.take(params.loadSize)
                if (ready.size >= params.loadSize || ready.size >= total) ready
                else ready + window(ready.size, params.loadSize - ready.size)
                    .mapIndexed { index, row -> item(ready.size + index, row) }
            } else window(offset, params.loadSize).mapIndexed(item)
            hold(items)
            reading?.missed()?.let { missed(it) }
            LoadResult.Page(items,
                if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                if (pins) pinPage?.nextOffset?.toInt() else if (offset + items.size >= total) null else offset + items.size,
                itemsBefore = if (pins) LoadResult.Page.COUNT_UNDEFINED else offset,
                itemsAfter = if (pins) LoadResult.Page.COUNT_UNDEFINED else (total - offset - items.size).coerceAtLeast(0))
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
