package io.github.screwys.rufin.browse

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
    private val releaseGroupId: String? = null,
    private val embedded: Boolean = false,
) {
    var active by mutableStateOf(false)
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
    var sortSelection by mutableStateOf<AndroidSortSelection?>(null)
        private set
    private var capturedDisplay by mutableStateOf<AndroidBrowseDisplay?>(null)
    val displaySettings get() = if (artistAlbums) browse.artistAlbumsDisplay ?: capturedDisplay else capturedDisplay
    var breadcrumbs by mutableStateOf<List<AndroidBreadcrumb>>(emptyList())
        private set
    var musicFolders by mutableStateOf<AndroidMusicFolderState?>(null)
        private set
    private val artistAlbums get() = releaseGroupId != null ||
        ((location.kind in listOf("artist", "ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography") ||
            summary?.mode in listOf("ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography")) && options.category == "albums")
    var summary by mutableStateOf<AndroidDetailSummary?>(null)
        private set
    var detailTrackFields by mutableStateOf<List<AndroidSortField>>(emptyList())
        private set
    var releaseGroups by mutableStateOf<List<AndroidArtistReleaseGroup>>(emptyList())
        private set
    var loading by mutableStateOf(true)
        private set
    private var revision by mutableIntStateOf(0)
    private var currentQuery by mutableStateOf<AndroidBrowseQuery?>(null)
    val contentKey get() = currentQuery
    private var capturedLabelsVersion = browse.labelsVersion
    private val data = MutableStateFlow<PagingData<BrowseItem>>(PagingData.empty())
    val pages: Flow<PagingData<BrowseItem>> = data.cachedIn(scope)

    private val routeId get() = browse.routes.firstOrNull { it.route == location.route }?.id
    val covers get() = displaySettings?.layout == "Grid"
    val hasActiveFilters get() = options.filter.isNotBlank() ||
        (supportsFavoriteFilter && options.favoritesOnly) || (supportsDownloadedFilter && options.downloadedOnly)
    fun resetFilters() {
        Snapshot.withMutableSnapshot {
            options.filter = ""
            options.favoritesOnly = false
            options.downloadedOnly = false
        }
    }
    val supportsLetterIndex get() = !embedded && sortSelection?.id in listOf("Name", "Title", "Artist", "AlbumArtist", "Album", "Genre")
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
        val library: AndroidLibrary?, val source: String?, val catalog: ULong?,
        val route: String, val category: String, val filter: String, val sort: String,
        val descending: Boolean, val favoritesOnly: Boolean, val downloadedOnly: Boolean,
        val revision: Int, val artistAlbumsRevision: Int,
        val downloadsRevision: ULong?,
        val homeBlocks: String?,
    )

    fun refresh() { revision++ }
    suspend fun sourceUri(): String? = currentQuery?.detailSourceUri()
    fun chooseSort(field: String, direction: Boolean) = connection.runAction {
        withContext(Dispatchers.IO) { browse.library?.setBrowseSort(location.route, options.category, field, direction) }
        if (artistAlbums) {
            options.sort = ""
            browse.artistAlbumsChanged(orderChanged = true)
        } else {
            options.sort = field
            options.descending = direction
        }
    }
    fun chooseDisplay(layout: String, size: String, spacing: String) = connection.runAction {
        val previous = (options.displaySettings ?: displaySettings)?.layout
        val display = withContext(Dispatchers.IO) {
            browse.library?.setBrowseDisplay(location.route, options.category, layout, size, spacing)
        } ?: return@runAction
        if (artistAlbums) {
            options.displaySettings = null
            browse.artistAlbumsChanged(display)
        } else if ((previous == "Detail") != (display.layout == "Detail")) { options.displaySettings = null; refresh() }
        else options.displaySettings = display
    }

    fun playSection(id: String, shuffled: Boolean = false) = connection.runAction {
        val query = homeRows.firstOrNull { it.row.sectionId == id }?.query ?: return@runAction
        connection.connectedService().playRequest { query.playSection(id, "now", shuffled,
            homeRows.firstOrNull { it.row.sectionId == id }?.row?.section) }
    }

    suspend fun observe() = withContext(Dispatchers.IO) { coroutineScope {
        launch {
            snapshotFlow { Triple(active || embedded || pins, browse.labelsVersion, currentQuery) }
                .filter { it.first && it.third != null && it.second != capturedLabelsVersion }
                .distinctUntilChangedBy { it.second }
                .collectLatest { (_, labelsVersion, query) ->
                    if (query == null) return@collectLatest
                    try {
                    val fields = query.sortFields()
                    val selection = query.sortSelection()
                    val display = query.displaySettings()
                    val trackFields = query.detailTrackFields()
                    val detail = if (summary != null) query.detailSummary() else null
                    val groups = if (releaseGroups.isNotEmpty()) query.artistReleaseGroups() else emptyList()
                    val homeTitles = homeRows.map { it.row.sectionId }.distinct().associateWith(query::homeSectionTitle)
                    if (currentQuery !== query) return@collectLatest
                    Snapshot.withMutableSnapshot {
                        capturedLabelsVersion = labelsVersion
                        sortFields = fields
                        sortSelection = selection
                        capturedDisplay = display
                        detailTrackFields = trackFields
                        if (detail != null) summary = detail
                        if (groups.isNotEmpty()) releaseGroups = groups
                        homeRows = homeRows.map { item ->
                            homeTitles[item.row.sectionId]?.let { title -> item.copy(row = item.row.copy(section = title)) } ?: item
                        }
                    }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                }
        }
        snapshotFlow {
            if (options.preparing || (!active && !embedded && !pins)) null else PageContext(browse.library, connection.libraryState?.sourceId, connection.libraryState?.revision,
                location.route, options.category, options.filter, options.sort, options.descending, options.favoritesOnly, options.downloadedOnly,
                revision, if (artistAlbums) browse.artistAlbumsRevision else 0,
                if (options.downloadedOnly || browse.showDownloadedBadges) connection.downloads.state?.revision else null,
                if (location.kind == "Home") browse.homeBlocksFingerprint else null)
        }.filterNotNull().distinctUntilChanged().collectLatest { context ->
            val library = context.library ?: return@collectLatest
            loading = true
            try {
                val labelsVersion = browse.labelsVersion
                val kind = if (pins) "Pins" else library.routeKind(context.route)
                if (!pins && context.source == null && kind !in listOf("Playlists", "PlaylistDetail") &&
                    !(kind == "History" && context.category != "current")) return@collectLatest
                if (pins) library.importPlaylistPins()
                val query = when {
                    pins -> null
                    releaseGroupId != null -> library.artistReleaseQuery(context.route, releaseGroupId, context.filter, context.sort, context.descending)
                    else -> library.browse(context.route, context.category, context.filter, context.sort, context.descending, context.favoritesOnly, context.downloadedOnly)
                }
                val favoriteFilter = query?.supportsFavoriteFilter() == true
                val downloadedFilter = query?.supportsDownloadedFilter() == true
                val count = query?.count()?.toInt()
                val fields = query?.sortFields().orEmpty()
                val selection = query?.sortSelection()
                val display = query?.displaySettings()
                val detail = if (embedded) null else query?.detailSummary()
                val trackFields = query?.detailTrackFields().orEmpty()
                val crumbs = if (embedded) emptyList() else query?.breadcrumbs().orEmpty()
                val folders = if (kind == "Folders") library.musicFolders() else null
                val groups = if (detail?.mode in listOf("ArtistDetail", "AlbumArtistDetail", "ArtistDiscography", "AlbumArtistDiscography")) query?.artistReleaseGroups().orEmpty() else emptyList()
                val isHome = !pins && routeId == "Home"
                val home = if (isHome && query != null) {
                    val rows = mutableListOf<BrowseItem>()
                    var offset = 0UL
                    while (offset < (count ?: 0).toULong()) {
                        val page = query.page(offset, 64u)
                        if (page.isEmpty()) break
                        rows += page.mapIndexed { index, row -> BrowseItem(row, offset + index.toULong(), query, display, contextTitle = row.section) }
                        offset += page.size.toULong()
                    }
                    rows
                } else emptyList()
                Snapshot.withMutableSnapshot {
                    capturedLabelsVersion = labelsVersion
                    currentQuery = query
                    supportsFavoriteFilter = favoriteFilter
                    supportsDownloadedFilter = downloadedFilter
                    sortFields = fields
                    sortSelection = selection
                    capturedDisplay = display
                    summary = detail
                    detailTrackFields = trackFields
                    releaseGroups = groups
                    breadcrumbs = crumbs
                    musicFolders = folders
                    homeRows = home
                }
                val viewportReady = CompletableDeferred<Unit>()
                if (isHome) { viewportReady.complete(Unit); loading = false }
                coroutineScope {
                    Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
                        maxSize = 192, enablePlaceholders = !pins,
                        jumpThreshold = if (pins) PagingSource.LoadResult.Page.COUNT_UNDEFINED else 192)) {
                        BrowsePagingSource(library, query, count, pins, display, location.playbackTitle) { success ->
                            if (success && !viewportReady.isCompleted) {
                                Snapshot.withMutableSnapshot {
                                    scrollSections = emptyList()
                                }
                                viewportReady.complete(Unit)
                            }
                            loading = false
                        }
                    }.flow.collect { data.value = it }
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { connection.runAction { throw error } }
            finally { loading = false }
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
    private val loaded: (Boolean) -> Unit,
) : PagingSource<Int, BrowseItem>() {
    override val jumpingSupported = true
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, BrowseItem> = try {
        val offset = params.key ?: 0
        val pinPage = if (pins) library.pins(offset.toULong(), params.loadSize.toUInt()) else null
        val total = pinPage?.total?.toInt() ?: count ?: 0
        val rows = pinPage?.rows ?: query!!.page(offset.toULong(), params.loadSize.toUInt())
        loaded(true)
        LoadResult.Page(rows.mapIndexed { index, row -> BrowseItem(row, (offset + index).toULong(), query, display, fromPins = pins, contextTitle = contextTitle) },
            if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
            if (pins) pinPage?.nextOffset?.toInt() else if (offset + rows.size >= total) null else offset + rows.size,
            itemsBefore = if (pins) LoadResult.Page.COUNT_UNDEFINED else offset,
            itemsAfter = if (pins) LoadResult.Page.COUNT_UNDEFINED else (total - offset - rows.size).coerceAtLeast(0))
    } catch (cancelled: CancellationException) { throw cancelled }
    catch (error: Exception) { loaded(false); LoadResult.Error(error) }

    override fun getRefreshKey(state: PagingState<Int, BrowseItem>): Int? = state.anchorPosition?.let {
        (it - state.config.initialLoadSize / 2).coerceAtLeast(0)
    }
}
