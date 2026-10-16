package io.github.screwys.rufin.downloads

import androidx.compose.runtime.*
import androidx.paging.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.downloads.*
import io.github.screwys.rufin.shared.library.TrackView
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.shared.rufin_core.SourceSummary
import io.github.screwys.rufin.shared.sources.DownloadQuality
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

internal data class DownloadRow(val job: DownloadQueueItem, val preview: TrackView?)
internal data class DownloadSource(val source: SourceSummary, val settings: SourceDownloadSettings)
internal fun DownloadRule.title() = translate(when (this) {
    DownloadRule.ENTIRE_LIBRARY -> "Entire Library"
    DownloadRule.FAVORITES -> "Favorites"
    DownloadRule.ALL_PLAYLISTS -> "All Playlists"
    DownloadRule.LATEST_FIVE_ALBUMS -> "5 Latest Albums"
})
internal fun DownloadRules.enabled(rule: DownloadRule) = when (rule) {
    DownloadRule.ENTIRE_LIBRARY -> entireLibrary
    DownloadRule.FAVORITES -> favorites
    DownloadRule.ALL_PLAYLISTS -> allPlaylists
    DownloadRule.LATEST_FIVE_ALBUMS -> latestFiveAlbums
}
internal fun DownloadSubject.title() = when (this) {
    is DownloadSubject.Rule -> v1.title()
    is DownloadSubject.Prepared -> title ?: translate("Downloads")
}
internal fun DownloadQueueState.title() = translate(when (this) {
    DownloadQueueState.QUEUED -> "Queued"
    DownloadQueueState.DOWNLOADING -> "Downloading"
    DownloadQueueState.WAITING_FOR_CONNECTION -> "Waiting for connection"
    DownloadQueueState.NEEDS_ATTENTION -> "Needs attention"
})

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@OptIn(ExperimentalCoroutinesApi::class)
internal class DownloadsConnection(private val model: RufinConnection, scope: CoroutineScope) {
    var state by mutableStateOf<DownloadStatus?>(null)
        private set
    var sources by mutableStateOf<List<DownloadSource>>(emptyList())
        private set
    var queueVisible by mutableStateOf(false)
    private var bridge by mutableStateOf<Downloads?>(null)
    private var queueSource: PagingSource<Int, DownloadRow>? = null

    val pages: Flow<PagingData<DownloadRow>> = snapshotFlow { bridge }.flatMapLatest { owner ->
        if (owner == null) flowOf(PagingData.empty()) else Pager(PagingConfig(pageSize = 32, initialLoadSize = 32,
            prefetchDistance = 12, maxSize = 96, enablePlaceholders = false)) {
            object : PagingSource<Int, DownloadRow>() {
                override suspend fun load(params: LoadParams<Int>): LoadResult<Int, DownloadRow> = try {
                    withContext(Dispatchers.Default) {
                        val offset = params.key ?: 0
                        val page = owner.queuePage(offset.toULong(), params.loadSize.toUInt())
                        LoadResult.Page(page.jobs.map { job -> DownloadRow(job, page.previews.find { it.mediaUri == job.previewUris.firstOrNull() }) }, if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                            if (offset.toULong() + page.jobs.size.toULong() >= page.total) null else offset + page.jobs.size)
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { LoadResult.Error(error) }
                override fun getRefreshKey(state: PagingState<Int, DownloadRow>): Int? = state.anchorPosition?.let {
                    val page = state.closestPageToPosition(it)
                    page?.prevKey?.plus(state.config.pageSize) ?: page?.nextKey?.minus(state.config.pageSize)
                }
            }.also { queueSource = it }
        }.flow
    }.cachedIn(scope)

    init {
        scope.launch {
            snapshotFlow { model.ready to model.service }.collectLatest { (ready, service) ->
                bridge = null; state = null; sources = emptyList()
                if (!ready || service == null) return@collectLatest
                val owner = service.runtime.value?.getOrNull()?.downloads() ?: return@collectLatest
                val changes = owner.subscribe()
                bridge = owner
                try {
                    refreshSources()
                    while (isActive) {
                        val next = withContext(Dispatchers.Default) { changes.next() }
                        state = next
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { model.runAction { throw error } }
                finally { bridge = null; changes.destroy(); owner.destroy() }
            }
        }
        scope.launch { snapshotFlow { model.sources }.drop(1).collectLatest { refreshSources() } }
        scope.launch {
            var revision: ULong? = null
            snapshotFlow { queueVisible to state?.queueRevision }.collect { (visible, next) ->
                if (visible && next != revision) { revision = next; queueSource?.invalidate() }
            }
        }
    }

    private suspend fun <T> withSettings(action: (SettingsOwner) -> T) = withContext(Dispatchers.IO) {
        model.connectedService().runtime.value!!.getOrThrow().settings().use(action)
    }
    suspend fun refreshSources() {
        if (bridge == null) return
        sources = withSettings { settings ->
            model.sources?.sources.orEmpty().filter { it.kind != "local" }
                .map { DownloadSource(it, settings.downloadSettings(it.id)) }
        }
    }
    fun pause(paused: Boolean) = model.runAction { bridge?.setPaused(paused) }
    fun cancel(job: DownloadQueueItem) = model.runAction { bridge?.cancelJob(job.sourceId, job.id) }
    fun rule(source: String, rule: DownloadRule, enabled: Boolean, deleteDownloads: Boolean = false) = model.runAction {
        withSettings { it.setDownloadRule(source, rule, enabled) }; if (!enabled) bridge?.removeRule(source, rule, deleteDownloads); refreshSources()
    }
    fun quality(source: String, quality: DownloadQuality) = model.runAction { withSettings { it.setDownloadQuality(source, quality) }; refreshSources() }
    fun directory(source: String, uri: String?) = model.runAction {
        withSettings { it.setDownloadLocation(source, uri?.let { value -> DownloadDirectory.Document(value) }) }
        refreshSources()
    }
    fun clear(source: String) = model.runAction { withSettings { it.clearDownloadRules(source) }; bridge?.clear(source, true); refreshSources() }
}
