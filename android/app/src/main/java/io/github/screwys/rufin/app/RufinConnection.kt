package io.github.screwys.rufin.app

import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.more.MoreConnection
import io.github.screwys.rufin.connect.ConnectConnection
import io.github.screwys.rufin.downloads.DownloadsConnection
import io.github.screwys.rufin.sources.SourceSetupConnection
import io.github.screwys.rufin.RufinService

import android.app.Application
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.net.Uri
import androidx.compose.runtime.*
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.media3.common.util.UnstableApi
import io.github.screwys.rufin.core.AndroidLibrary
import io.github.screwys.rufin.core.AndroidLibraryState
import io.github.screwys.rufin.core.AndroidLibraryEvents
import io.github.screwys.rufin.core.AndroidFavoriteSettlement
import io.github.screwys.rufin.core.AndroidSourceState
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.userMessage
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

/** Keeps library paging, service binding and user actions across Activity recreation. */
@androidx.annotation.OptIn(UnstableApi::class)
@OptIn(FlowPreview::class, ExperimentalCoroutinesApi::class)
class RufinConnection(application: Application) : AndroidViewModel(application) {
    internal data class FeedbackMessage(val message: String, val artworkIdentity: ByteArray?)
    internal var feedback by mutableStateOf<FeedbackMessage?>(null)
        private set
    internal var controlNotifications = true
    private var feedbackTimeout: Job? = null
    internal fun showFeedback(message: String, artworkIdentity: ByteArray? = null, control: Boolean = false) {
        if (control && !controlNotifications) return
        feedbackTimeout?.cancel()
        feedback = FeedbackMessage(message, artworkIdentity)
        feedbackTimeout = viewModelScope.launch { delay(2000); feedback = null }
    }
    internal fun showFeedback(failure: Throwable) = showFeedback(failure.userMessage())
    internal var libraryEvents: AndroidLibraryEvents? = null
        private set
    internal var favoriteRevision by mutableLongStateOf(0)
        private set
    private val settlements = MutableSharedFlow<AndroidFavoriteSettlement>(extraBufferCapacity = 32)
    internal val favoriteSettlements = settlements.asSharedFlow()
    internal fun projectedFavorite(kind: String, uri: String, fallback: Boolean): Boolean {
        favoriteRevision
        return libraryEvents?.projectedFavorite(kind, uri, fallback) ?: fallback
    }
    internal fun setFavoriteIntent(kind: String, uri: String, value: Boolean) {
        libraryEvents?.setFavoriteIntent(kind, uri, value)
        favoriteRevision++
    }
    internal fun releaseFavoriteIntent(kind: String, uri: String, requested: Boolean) {
        libraryEvents?.releaseFavoriteIntent(kind, uri, requested)
        favoriteRevision++
    }
    internal var overviewExportName by mutableStateOf<String?>(null)
        private set
    private var overviewExportBytes: ByteArray? = null
    internal fun prepareOverviewExport(bytes: ByteArray, name: String) {
        overviewExportBytes = bytes
        overviewExportName = name
    }
    internal fun overviewExportLaunched() { overviewExportName = null }
    internal fun saveOverview(uri: Uri?) {
        val bytes = overviewExportBytes
        overviewExportBytes = null
        if (uri == null || bytes == null) return
        runAction {
            withContext(Dispatchers.IO) {
                getApplication<Application>().contentResolver.openOutputStream(uri)?.use { it.write(bytes) }
                    ?: error(translate("This isn't available"))
            }
        }
    }
    internal val browse: BrowseConnection
    internal val more: MoreConnection
    internal val connect: ConnectConnection
    internal val downloads: DownloadsConnection
    private var sourceSetup: SourceSetupConnection? = null
    private var sourceSetupId: String? = null
    internal fun sourceSetupSession(id: String?): SourceSetupConnection {
        if (sourceSetup == null || sourceSetupId != id) {
            sourceSetup?.close()
            sourceSetup = SourceSetupConnection(this, id)
            sourceSetupId = id
        }
        return sourceSetup!!
    }
    internal fun closeSourceSetup() { sourceSetup?.close(); sourceSetup = null; sourceSetupId = null }
    var service by mutableStateOf<RufinService?>(null)
        private set
    var ready by mutableStateOf(false)
        private set
    var startupError by mutableStateOf<String?>(null)
        private set
    var pendingActions by mutableIntStateOf(0)
        private set
    var libraryState by mutableStateOf<AndroidLibraryState?>(null)
        private set
    var sources by mutableStateOf<AndroidSourceState?>(null)
        private set
    var debugLogging by mutableStateOf(false)
        private set
    private var observation: Job? = null

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            val connected = (binder as RufinService.Connection).service
            service = connected
            observe(connected)
        }
        override fun onServiceDisconnected(name: ComponentName) {
            observation?.cancel()
            service = null
            ready = false
            libraryState = null
            sources = null
            debugLogging = false
        }
    }

    init {
        connect = ConnectConnection(this)
        downloads = DownloadsConnection(this, viewModelScope)
        browse = BrowseConnection(this, viewModelScope)
        more = MoreConnection(this, viewModelScope)
        viewModelScope.launch { connect.observe() }
        application.bindService(Intent(application, RufinService::class.java)
            .setAction(RufinService.ACTION_BIND_RUNTIME), connection, Context.BIND_AUTO_CREATE)
    }

    private fun observe(connected: RufinService) {
        observation?.cancel()
        observation = viewModelScope.launch {
            connected.runtime.collectLatest { result ->
                ready = false
                libraryState = null
                sources = null
                debugLogging = false
                startupError = result?.exceptionOrNull()?.userMessage()
                val core = result?.getOrNull() ?: return@collectLatest
                val library = core.library()
                val events = core.libraryEvents()
                libraryEvents = events
                val changes = library.subscribe()
                val sourceChanges = core.subscribeSources()
                debugLogging = core.debugLogging()
                try {
                    libraryState = changes.next()
                    ready = true
                    coroutineScope {
                        launch { connected.playbackNotices.collect { showFeedback(it) } }
                        launch {
                            while (isActive) {
                                val event = events.next()
                                favoriteRevision++
                                event.favorite?.let { settlements.emit(it) }
                                event.notice?.let { showFeedback(it) }
                            }
                        }
                        launch {
                            while (isActive) sources = sourceChanges.next()
                        }
                        while (isActive) {
                            val state = changes.next()
                            if (state.error != libraryState?.error) state.error?.let { showFeedback(it) }
                            libraryState = state
                        }
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    showFeedback(error)
                } finally {
                    ready = false
                    libraryState = null
                    sources = null
                    changes.destroy()
                    sourceChanges.destroy()
                    library.destroy()
                    libraryEvents = null
                    events.destroy()
                }
            }
        }
    }

    fun runAction(action: suspend () -> Unit) {
        viewModelScope.launch {
            pendingActions++
            try { action() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { showFeedback(error) }
            finally { pendingActions-- }
        }
    }

    suspend fun connectedService(): RufinService {
        val connected = snapshotFlow { service }.filterNotNull().first()
        connected.runtime.filterNotNull().first().getOrThrow()
        return connected
    }

    fun openDocuments(uris: List<Uri>, flags: Int) = runAction {
        uris.forEach { retainGrant(it, flags) }
        connectedService().openUris(uris.map(Uri::toString))
    }

    fun addFolder(uri: Uri, flags: Int) = addLocalSource { connected ->
        retainGrant(uri, flags)
        connected.runtime.value!!.getOrThrow().addDocumentRoot(uri.toString())
    }

    fun addMediaLibrary() = addLocalSource { it.addMediaLibrary() }

    suspend fun addMediaLibraryForSetup(): Boolean {
        val service = connectedService()
        if (!service.addMediaLibrary()) return false
        selectLocalSource(service)
        return true
    }

    suspend fun addFolderForSetup(uri: Uri) {
        getApplication<Application>().contentResolver.takePersistableUriPermission(uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        val service = connectedService()
        service.runtime.value!!.getOrThrow().addDocumentRoot(uri.toString())
        selectLocalSource(service)
    }

    private suspend fun selectLocalSource(service: RufinService) {
        val core = service.runtime.value!!.getOrThrow()
        core.subscribeSources().use { subscription ->
            val state = subscription.next()
            state.sources.firstOrNull { it.kind == "local" }?.let { local ->
                if (state.selectedSourceId != local.id) core.library().use { it.selectSource(local.id) }
            }
        }
    }

    private fun addLocalSource(add: suspend (RufinService) -> Unit) = runAction {
        val connected = connectedService()
        add(connected)
        val core = connected.runtime.value!!.getOrThrow()
        val subscription = core.subscribeSources()
        try {
            val state = subscription.next()
            val local = state.sources.firstOrNull { it.kind == "local" }
            if (local != null && state.selectedSourceId != local.id) {
                core.library().use { it.selectSource(local.id) }
            }
        } finally { subscription.destroy() }
    }

    private fun retainGrant(uri: Uri, flags: Int) {
        if (flags and Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION != 0) {
            getApplication<Application>().contentResolver.takePersistableUriPermission(uri,
                flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION))
        }
    }

    fun requestLocalNetwork() = runAction { connectedService().requestLocalNetwork() }
    internal fun openOverview(item: io.github.screwys.rufin.core.AndroidOverviewItem, period: String) = runAction {
        val library = browse.library ?: return@runAction
        if (item.detailRoute != null) {
            item.sourceId?.let { library.selectSource(it) }
            browse.openRoute(item.detailRoute!!, item.title, item.kind)
        } else connectedService().playRequest {
            library.playRows("overview:$period:${item.mediaUri}", listOf(item.mediaUri), 0UL, "now", false, translate("Listening overview"))
        }
    }

    fun changeDebugLogging(enabled: Boolean) = runAction {
        val core = connectedService().runtime.value!!.getOrThrow()
        try { core.setDebugLogging(enabled) }
        finally { debugLogging = core.debugLogging() }
    }

    fun saveDiagnosticLog(uri: Uri) = runAction {
        val core = connectedService().runtime.value!!.getOrThrow()
        withContext(Dispatchers.IO) {
            val contents = core.diagnosticLog()
            val output = getApplication<Application>().contentResolver.openOutputStream(uri, "wt")
                ?: error(translate("Could not save diagnostic log"))
            output.bufferedWriter().use { it.write(contents) }
        }
    }

    fun refresh() = runAction {
        connectedService().runtime.value!!.getOrThrow().library().use { it.refresh() }
    }

    fun selectSource(id: String) = runAction {
        connectedService().runtime.value!!.getOrThrow().library().use { it.selectSource(id) }
    }

    fun retryStartup() {
        startupError = null
        service?.retryStartup()
    }


    override fun onCleared() {
        closeSourceSetup()
        observation?.cancel()
        getApplication<Application>().unbindService(connection)
    }
}
