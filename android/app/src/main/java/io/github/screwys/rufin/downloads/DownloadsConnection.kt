package io.github.screwys.rufin.downloads

import androidx.compose.runtime.*
import androidx.paging.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@OptIn(ExperimentalCoroutinesApi::class)
internal class DownloadsConnection(private val model: RufinConnection, scope: CoroutineScope) {
    var state by mutableStateOf<AndroidDownloadsState?>(null)
        private set
    var sources by mutableStateOf<List<AndroidDownloadSource>>(emptyList())
        private set
    var queueVisible by mutableStateOf(false)
    private var bridge by mutableStateOf<AndroidDownloads?>(null)
    private var queueSource: PagingSource<Int, AndroidDownloadJob>? = null

    val pages: Flow<PagingData<AndroidDownloadJob>> = snapshotFlow { bridge }.flatMapLatest { owner ->
        if (owner == null) flowOf(PagingData.empty()) else Pager(PagingConfig(pageSize = 32, initialLoadSize = 32,
            prefetchDistance = 12, maxSize = 96, enablePlaceholders = false)) {
            object : PagingSource<Int, AndroidDownloadJob>() {
                override suspend fun load(params: LoadParams<Int>): LoadResult<Int, AndroidDownloadJob> = try {
                    val offset = params.key ?: 0
                    val page = owner.queuePage(offset.toULong(), params.loadSize.toUInt())
                    LoadResult.Page(page.jobs, if (offset == 0) null else (offset - params.loadSize).coerceAtLeast(0),
                        if (offset.toULong() + page.jobs.size.toULong() >= page.total) null else offset + page.jobs.size)
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { LoadResult.Error(error) }
                override fun getRefreshKey(state: PagingState<Int, AndroidDownloadJob>): Int? = state.anchorPosition?.let {
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
                    sources = owner.sources()
                    while (isActive) {
                        val next = changes.next()
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

    suspend fun refreshSources() { bridge?.let { sources = it.sources() } }
    fun pause(paused: Boolean) = model.runAction { bridge?.setPaused(paused) }
    fun cancel(job: AndroidDownloadJob) = model.runAction { bridge?.cancel(job.sourceId, job.id) }
    fun rule(source: String, id: String, enabled: Boolean, deleteDownloads: Boolean = false) = model.runAction {
        bridge?.setRule(source, id, enabled, deleteDownloads); refreshSources()
    }
    fun quality(source: String, bitrate: UInt?) = model.runAction { bridge?.setQuality(source, bitrate); refreshSources() }
    fun directory(source: String, uri: String?) = model.runAction {
        model.connectedService().runtime.value?.getOrThrow()?.setDownloadDocumentDirectory(source, uri)
        refreshSources()
    }
    fun clear(source: String) = model.runAction { bridge?.clearSource(source); refreshSources() }
}
