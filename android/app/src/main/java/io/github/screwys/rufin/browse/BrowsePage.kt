package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.rufin_core.CatalogRevision
import io.github.screwys.rufin.shared.rufin_core.LibraryField
import io.github.screwys.rufin.shared.rufin_core.LibraryLayout
import io.github.screwys.rufin.shared.rufin_core.DisplaySize

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.runtime.*
import androidx.compose.runtime.snapshots.Snapshot
import androidx.paging.*
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class BrowsePage(
    private val connection: RufinConnection,
    private val browse: BrowseConnection,
    private val location: BrowseLocation,
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
    var homeRows by mutableStateOf<List<BrowseItem>>(emptyList())
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
    private val artistAlbums get() =
        ((location.kind in listOf("artist", "ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography") ||
            summary?.mode?.kind in listOf("ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography")) && options.category == "albums")
    var summary by mutableStateOf<AndroidDetailSummary?>(null)
        private set
    var detailTrackFields by mutableStateOf<List<AndroidSortField>>(emptyList())
        private set
    var loading by mutableStateOf(true)
        private set
    private var revision by mutableIntStateOf(0)
    private var currentQuery by mutableStateOf<AndroidBrowseQuery?>(null)
    private var preparedPage by mutableStateOf<AndroidBrowsePage?>(null)
    private var preparedContext by mutableStateOf<PageContext?>(null)
    var downloadRevision by mutableStateOf<ULong?>(null)
        private set
    private var capturedLabelsVersion = browse.labelsVersion
    private val data = MutableStateFlow<PagingData<BrowseItem>>(PagingData.empty())
    val pages: Flow<PagingData<BrowseItem>> = data.cachedIn(scope)

    private val routeId get() = browse.routes.firstOrNull { it.route == location.route }?.id
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
    val supportsLetterIndex get() = !embedded && sortSelection?.sortKey in listOf(LibraryField.TITLE, LibraryField.ARTIST, LibraryField.ALBUM_ARTIST, LibraryField.ALBUM, LibraryField.GENRE)
    suspend fun letterPosition(letter: String): ULong? {
        val query = currentQuery ?: return null
        val sections = if (scrollSections.isEmpty()) query.sectionPositions() else scrollSections
        if (currentQuery !== query) return null
        scrollSections = sections
        return sections.firstOrNull { it.title == letter }?.index
            ?: sections.filter { it.title <= letter }.maxByOrNull { it.title }?.index
            ?: sections.firstOrNull()?.index
    }

    private data class PageContext(
        val library: AndroidLibrary?, val sourceId: String?, val catalog: CatalogRevision?,
        val route: Route, val category: String, val filter: String, val sort: LibraryField?,
        val descending: Boolean, val favoritesOnly: Boolean, val downloadedOnly: Boolean,
        val revision: Int, val artistAlbumsRevision: Int,
        val downloadsRevision: ULong?,
        val homeBlocks: String?,
        val parentPage: AndroidBrowsePage?,
    )

    fun refresh() { loading = true; revision++ }
    suspend fun sourceUri(): String? = currentQuery?.detailSourceUri()
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
        if (artistAlbums) {
            options.displaySettings = null
            browse.artistAlbumsChanged(display)
        } else if ((previous == LibraryLayout.DETAIL) != (display.layout == LibraryLayout.DETAIL)) { options.displaySettings = null; refresh() }
        else options.displaySettings = display
    }

    fun playSection(id: String, shuffled: Boolean = false) = connection.runAction {
        val query = homeRows.firstOrNull { it.row.sectionId == id }?.query ?: return@runAction
        connection.connectedService().playRequest { query.playSection(id, "now", shuffled,
            homeRows.firstOrNull { it.row.sectionId == id }?.row?.section) }
    }

    @OptIn(ExperimentalCoroutinesApi::class)
    suspend fun observe(): Unit = withContext(Dispatchers.IO) { coroutineScope {
        val lifetime = coroutineContext
        fun sameContext(previous: PageContext?, current: PageContext?): Boolean =
            previous != null && current != null && previous.copy(catalog = current.catalog) == current &&
                previous.catalog?.catalog == current.catalog?.catalog &&
                ((if (pins) browse.library?.pinsUseActivity() == false else currentQuery?.activitySensitive() == false) ||
                    previous.catalog?.activity == current.catalog?.activity)
        var favoriteJob: Job? = null
        launch {
            snapshotFlow { Triple(parent?.active ?: active, browse.labelsVersion, currentQuery) }
                .collectLatest { (active, labelsVersion, query) ->
                    if (!active || query == null || labelsVersion == capturedLabelsVersion) return@collectLatest
                    try {
                    val fields = query.sortFields()
                    val display = query.displaySettings()
                    val trackFields = query.detailTrackFields()
                    val homeTitles = homeRows.map { it.row.sectionId }.distinct().associateWith(query::homeSectionTitle)
                    if (currentQuery !== query) return@collectLatest
                    Snapshot.withMutableSnapshot {
                        capturedLabelsVersion = labelsVersion
                        sortFields = fields
                        capturedDisplay = display
                        detailTrackFields = trackFields
                        summary = preparedPage?.summary?.let { detail ->
                            detail.copy(label = translate(detail.label), links = detail.links.map { it.copy(title = translate(it.title)) })
                        }
                        releaseGroups = preparedPage?.groups.orEmpty().map { it.copy(title = translate(it.title)) }
                        homeRows = homeRows.map { item ->
                            homeTitles[item.row.sectionId]?.let { title -> item.copy(row = item.row.copy(section = title)) } ?: item
                        }
                    }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                }
        }
        snapshotFlow {
            if (!(parent?.active ?: active) || (parent != null && parent.preparedContext == null)) null else PageContext(browse.library, connection.sources?.selectedSourceId,
                if (parent != null) parent.preparedContext?.catalog else connection.catalogRevision,
                location.route, options.category, options.filter, options.sort, options.descending, options.favoritesOnly, options.downloadedOnly,
                revision + (parent?.revision ?: 0), if (artistAlbums) browse.artistAlbumsRevision else 0,
                if (options.downloadedOnly) connection.downloads.state?.revision ?: 0UL else null,
                if (location.kind == "Home") browse.homeBlocksFingerprint else null, parent?.preparedPage)
        }.distinctUntilChanged(::sameContext).mapLatest { context ->
            val library = context?.library ?: return@mapLatest null
            if (sameContext(preparedContext, context)) return@mapLatest null
            loading = true
            try {
                val labelsVersion = browse.labelsVersion
                val kind = if (pins) "Pins" else context.route.kind
                if (!pins && context.sourceId == null && kind !in listOf("Playlists", "PlaylistDetail", "SmartPlaylists", "SmartPlaylistDetail", "PlaylistFolder") &&
                    !(kind == "History" && context.category != "current")) return@mapLatest null
                if (pins) library.importPlaylistPins()
                val query = if (pins) null else library.browse(context.route, context.category, context.filter, context.sort, context.descending, context.favoritesOnly, context.downloadedOnly)
                val favoriteFilter = query?.supportsFavoriteFilter() == true
                val downloadedFilter = query?.supportsDownloadedFilter() == true
                val favoriteSeed = context.parentPage?.takeIf {
                    context.filter == parent?.preparedContext?.filter && !context.favoritesOnly && !context.downloadedOnly
                }
                val downloadRevision = if (favoriteSeed != null) parent?.downloadRevision else connection.downloads.state?.revision
                val prepared = if (favoriteSeed != null) null else query?.initialPage(embedded)
                val count = favoriteSeed?.summary?.favoriteTotal?.toInt() ?: prepared?.count?.toInt()
                val seedRows = if (context.sort == null) favoriteSeed?.favoriteRows ?: prepared?.rows else prepared?.rows
                val fields = query?.sortFields().orEmpty()
                val display = query?.displaySettings()
                val detail = prepared?.summary
                val trackFields = query?.detailTrackFields().orEmpty()
                val crumbs = if (embedded) emptyList() else query?.breadcrumbs().orEmpty()
                val folders = if (kind == "Folders") library.musicFolders() else null
                val isHome = !pins && routeId == "Home"
                val home = if (isHome && query != null) {
                    val rows = mutableListOf<BrowseItem>()
                    rows += prepared?.rows.orEmpty().mapIndexed { index, row -> BrowseItem(row, index.toULong(), query, display, contextTitle = row.section, downloadRevision = downloadRevision) }
                    var offset = rows.size.toULong()
                    while (offset < (count ?: 0).toULong()) {
                        val pageDownloadRevision = connection.downloads.state?.revision
                        val page = query.page(offset, 64u)
                        if (page.isEmpty()) break
                        rows += page.mapIndexed { index, row -> BrowseItem(row, offset + index.toULong(), query, display, contextTitle = row.section, downloadRevision = pageDownloadRevision) }
                        offset += page.size.toULong()
                    }
                    rows
                } else emptyList()
                val favoriteRoute = detail?.favoriteRoute?.takeIf {
                    detail.showsReleases && detail.mode.kind in listOf("ArtistDetail", "AlbumArtistDetail")
                }
                val favorite = if (favoritesPage?.location?.route == favoriteRoute) favoritesPage else {
                    favoriteJob?.cancel()
                    favoriteJob = null
                    favoriteRoute?.let { route ->
                        val childJob = Job(lifetime[Job])
                        val childScope = CoroutineScope(lifetime + childJob)
                        val childOptions = BrowseOptions().also { it.filter = context.filter }
                        val childLocation = BrowseLocation(route, "Favorite tracks", "ArtistFavoriteTracks", childOptions, location.playbackTitle)
                        BrowsePage(connection, browse, childLocation, childOptions, childScope, parent = this@BrowsePage).also { page ->
                            favoriteJob = childJob
                            childScope.launch { page.observe() }
                        }
                    }
                }
                Snapshot.withMutableSnapshot {
                    capturedLabelsVersion = labelsVersion
                    this@BrowsePage.downloadRevision = downloadRevision
                    currentQuery = query
                    preparedPage = prepared
                    preparedContext = if (isHome) context else null
                    scrollSections = emptyList()
                    supportsFavoriteFilter = favoriteFilter
                    supportsDownloadedFilter = downloadedFilter
                    sortFields = fields
                    capturedDisplay = display
                    summary = detail
                    detailTrackFields = trackFields
                    breadcrumbs = crumbs
                    musicFolders = folders
                    homeRows = home
                    favorite?.let { page ->
                        if (page.options.filter != context.filter) page.loading = true
                        page.options.filter = context.filter
                    }
                    favoritesPage = favorite
                    releaseGroups = prepared?.groups.orEmpty()
                }
                if (isHome) return@mapLatest null
                context to Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
                    maxSize = 192, enablePlaceholders = !pins,
                    jumpThreshold = if (pins) PagingSource.LoadResult.Page.COUNT_UNDEFINED else 192)) {
                    BrowsePagingSource(library, query, count, pins, display, location.playbackTitle, seedRows, downloadRevision, connection.downloads)
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
    private val query: AndroidBrowseQuery?,
    private val count: Int?,
    private val pins: Boolean,
    private val display: AndroidBrowseDisplay?,
    private val contextTitle: String,
    private val firstRows: List<AndroidBrowseRow>?,
    private val firstDownloadRevision: ULong?,
    private val downloads: io.github.screwys.rufin.downloads.DownloadsConnection,
) : PagingSource<Int, BrowseItem>() {
    override val jumpingSupported = true
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, BrowseItem> = try {
        withContext(Dispatchers.Default) {
            val offset = params.key ?: 0
            val downloadRevision = if (offset == 0 && firstRows != null) firstDownloadRevision else downloads.state?.revision
            val pinPage = if (pins) library.pins(offset.toULong(), params.loadSize.toUInt()) else null
            val total = pinPage?.total?.toInt() ?: count ?: 0
            val rows = pinPage?.rows ?: if (offset == 0 && firstRows != null) {
                val ready = firstRows.take(params.loadSize)
                if (ready.size >= params.loadSize || ready.size >= total) ready
                else ready + query!!.page(ready.size.toULong(), (params.loadSize - ready.size).toUInt())
            } else query!!.page(offset.toULong(), params.loadSize.toUInt())
            LoadResult.Page(rows.mapIndexed { index, row -> BrowseItem(row, (offset + index).toULong(), query, display, fromPins = pins, contextTitle = contextTitle, downloadRevision = downloadRevision) },
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
