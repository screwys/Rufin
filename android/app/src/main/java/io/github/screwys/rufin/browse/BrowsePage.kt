package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.rufin_core.ConfiguredSources
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
    private val releaseGroupId: String? = null,
    private val embedded: Boolean = false,
    private val parent: BrowsePage? = null,
) {
    var active by mutableStateOf(false)
    var favoritesPage by mutableStateOf<BrowsePage?>(null)
        private set
    var releasePages by mutableStateOf<List<Pair<AndroidArtistReleaseGroup, BrowsePage>>>(emptyList())
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
    private val artistAlbums get() = releaseGroupId != null ||
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
        val library: AndroidLibrary?, val source: ConfiguredSources?, val catalog: CatalogRevision?,
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

    suspend fun observe(): Unit = withContext(Dispatchers.IO) { coroutineScope {
        val childJobs = mutableMapOf<BrowsePage, Job>()
        fun childPage(childLocation: BrowseLocation, groupId: String? = null): BrowsePage {
            val childJob = Job(coroutineContext[Job])
            val childScope = CoroutineScope(coroutineContext + childJob)
            val page = BrowsePage(connection, browse, childLocation, childLocation.options, childScope,
                releaseGroupId = groupId, embedded = true, parent = this@BrowsePage)
            childJobs[page] = childJob
            childScope.launch { page.observe() }
            return page
        }
        launch {
            snapshotFlow { Triple(parent?.active ?: active, browse.labelsVersion, currentQuery) }
                .filter { it.first && it.third != null && it.second != capturedLabelsVersion }
                .distinctUntilChangedBy { it.second }
                .collectLatest { (_, labelsVersion, query) ->
                    if (query == null) return@collectLatest
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
                        releasePages = releasePages.map { (group, owner) ->
                            (preparedPage?.groups?.firstOrNull { it.id == group.id }?.let { it.copy(title = translate(it.title)) } ?: group) to owner
                        }
                        homeRows = homeRows.map { item ->
                            homeTitles[item.row.sectionId]?.let { title -> item.copy(row = item.row.copy(section = title)) } ?: item
                        }
                    }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { connection.runAction { throw error } }
                }
        }
        snapshotFlow {
            if (!(parent?.active ?: active) || (parent != null && parent.preparedContext == null)) null else PageContext(browse.library, connection.sources,
                if (parent != null) parent.preparedContext?.catalog else connection.catalogRevision,
                location.route, options.category, options.filter, options.sort, options.descending, options.favoritesOnly, options.downloadedOnly,
                revision + (parent?.revision ?: 0), if (artistAlbums) browse.artistAlbumsRevision else 0,
                if (options.downloadedOnly || browse.showDownloadedBadges) connection.downloads.state?.revision ?: 0UL else null,
                if (location.kind == "Home") browse.homeBlocksFingerprint else null, parent?.preparedPage)
        }.filterNotNull().distinctUntilChanged { previous, current ->
            previous.copy(catalog = current.catalog) == current &&
                previous.catalog?.catalog == current.catalog?.catalog &&
                ((if (pins) browse.library?.pinsUseActivity() == false else currentQuery?.activitySensitive() == false) ||
                    previous.catalog?.activity == current.catalog?.activity)
        }.collectLatest { context ->
            val library = context.library ?: return@collectLatest
            loading = true
            try {
                val labelsVersion = browse.labelsVersion
                val kind = if (pins) "Pins" else context.route.kind
                if (!pins && context.source?.selectedSourceId == null && kind !in listOf("Playlists", "PlaylistDetail") &&
                    !(kind == "History" && context.category != "current")) return@collectLatest
                if (pins) library.importPlaylistPins()
                val query = when {
                    pins -> null
                    releaseGroupId != null -> parent?.currentQuery?.releaseQuery(releaseGroupId, context.filter, context.sort, context.descending) ?: return@collectLatest
                    else -> library.browse(context.route, context.category, context.filter, context.sort, context.descending, context.favoritesOnly, context.downloadedOnly)
                }
                val favoriteFilter = query?.supportsFavoriteFilter() == true
                val downloadedFilter = query?.supportsDownloadedFilter() == true
                val parentSeed = context.parentPage?.takeIf {
                    context.filter == parent?.preparedContext?.filter && !context.favoritesOnly && !context.downloadedOnly
                }
                val groupSeed = parentSeed?.groups?.firstOrNull { it.id == releaseGroupId }
                val favoriteSeed = parentSeed?.takeIf { releaseGroupId == null }
                val prepared = if (groupSeed != null || favoriteSeed != null) null else query?.initialPage(embedded)
                val count = groupSeed?.total?.toInt() ?: favoriteSeed?.summary?.favoriteTotal?.toInt() ?: prepared?.count?.toInt()
                val seedRows = if (context.sort == null) groupSeed?.firstRows ?: favoriteSeed?.favoriteRows ?: prepared?.rows else prepared?.rows
                val fields = query?.sortFields().orEmpty()
                val display = query?.displaySettings()
                val detail = prepared?.summary
                val trackFields = query?.detailTrackFields().orEmpty()
                val crumbs = if (embedded) emptyList() else query?.breadcrumbs().orEmpty()
                val folders = if (kind == "Folders") library.musicFolders() else null
                val groups = prepared?.groups.orEmpty()
                val isHome = !pins && routeId == "Home"
                val home = if (isHome && query != null) {
                    val rows = mutableListOf<BrowseItem>()
                    rows += prepared?.rows.orEmpty().mapIndexed { index, row -> BrowseItem(row, index.toULong(), query, display, contextTitle = row.section) }
                    var offset = rows.size.toULong()
                    while (offset < (count ?: 0).toULong()) {
                        val page = query.page(offset, 64u)
                        if (page.isEmpty()) break
                        rows += page.mapIndexed { index, row -> BrowseItem(row, offset + index.toULong(), query, display, contextTitle = row.section) }
                        offset += page.size.toULong()
                    }
                    rows
                } else emptyList()
                val favorite = detail?.favoriteRoute?.takeIf {
                    detail.showsReleases && detail.mode.kind in listOf("ArtistDetail", "AlbumArtistDetail")
                }?.let { route ->
                    favoritesPage?.takeIf { it.location.route == route && it in childJobs } ?: run {
                        val childOptions = BrowseOptions().also { it.filter = context.filter }
                        childPage(BrowseLocation(route, "Favorite tracks", "ArtistFavoriteTracks", childOptions, location.playbackTitle))
                    }
                }
                val releases = groups.filter { it.total > 0UL }.map { group ->
                    val existing = releasePages.firstOrNull {
                        it.first.id == group.id && it.first.route == group.route && it.second in childJobs
                    }?.second
                    val page = existing ?: run {
                        val childOptions = BrowseOptions().also { it.category = "albums"; it.filter = context.filter }
                        childPage(BrowseLocation(group.route, translate(group.title), "ArtistDiscography", childOptions, location.playbackTitle), group.id)
                    }
                    group to page
                }
                childJobs.entries.removeAll { (page, job) ->
                    if (page !== favorite && releases.none { it.second === page }) { job.cancel(); true } else false
                }
                Snapshot.withMutableSnapshot {
                    capturedLabelsVersion = labelsVersion
                    currentQuery = query
                    preparedPage = prepared
                    preparedContext = context
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
                    childJobs.keys.forEach { page ->
                        if (page.options.filter != context.filter) page.loading = true
                        page.options.filter = context.filter
                    }
                    favoritesPage = favorite
                    releasePages = releases
                }
                if (isHome || detail?.showsReleases == true) return@collectLatest
                Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
                    maxSize = 192, enablePlaceholders = !pins,
                    jumpThreshold = if (pins) PagingSource.LoadResult.Page.COUNT_UNDEFINED else 192)) {
                    BrowsePagingSource(library, query, count, pins, display, location.playbackTitle, seedRows) {
                        loading = false
                    }
                }.flow.collect { data.value = it }
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
    private val firstRows: List<AndroidBrowseRow>?,
    private val loaded: () -> Unit,
) : PagingSource<Int, BrowseItem>() {
    override val jumpingSupported = true
    override suspend fun load(params: LoadParams<Int>): LoadResult<Int, BrowseItem> = try {
        val page = withContext(Dispatchers.Default) {
            val offset = params.key ?: 0
            val pinPage = if (pins) library.pins(offset.toULong(), params.loadSize.toUInt()) else null
            val total = pinPage?.total?.toInt() ?: count ?: 0
            val rows = pinPage?.rows ?: if (offset == 0 && firstRows != null) {
                val ready = firstRows.take(params.loadSize)
                if (ready.size >= params.loadSize || ready.size >= total) ready
                else ready + query!!.page(ready.size.toULong(), (params.loadSize - ready.size).toUInt())
            } else query!!.page(offset.toULong(), params.loadSize.toUInt())
            LoadResult.Page(rows.mapIndexed { index, row -> BrowseItem(row, (offset + index).toULong(), query, display, fromPins = pins, contextTitle = contextTitle) },
                if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                if (pins) pinPage?.nextOffset?.toInt() else if (offset + rows.size >= total) null else offset + rows.size,
                itemsBefore = if (pins) LoadResult.Page.COUNT_UNDEFINED else offset,
                itemsAfter = if (pins) LoadResult.Page.COUNT_UNDEFINED else (total - offset - rows.size).coerceAtLeast(0))
        }
        loaded()
        page
    } catch (cancelled: CancellationException) { throw cancelled }
    catch (error: Exception) { loaded(); LoadResult.Error(error) }

    override fun getRefreshKey(state: PagingState<Int, BrowseItem>): Int? = state.anchorPosition?.let {
        (it - state.config.initialLoadSize / 2).coerceAtLeast(0)
    }
}

internal val AndroidBrowseDisplay.layout get() = settings.layout
internal val AndroidBrowseDisplay.size get() = settings.display.size
internal val AndroidBrowseDisplay.gridSpacing get() = settings.display.gridSpacing
internal val AndroidBrowseDisplay.showHeader get() = settings.display.showHeader
