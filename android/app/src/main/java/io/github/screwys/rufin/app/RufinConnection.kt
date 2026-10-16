package io.github.screwys.rufin.app

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.rufin_core.CatalogRevision
import io.github.screwys.rufin.shared.library.sourceIdForUri

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
import io.github.screwys.rufin.core.AndroidLibraryEvents
import io.github.screwys.rufin.shared.rufin_core.FavoriteSettlement
import io.github.screwys.rufin.shared.rufin_core.FavoriteIntents
import io.github.screwys.rufin.browse.favoriteTarget
import io.github.screwys.rufin.shared.rufin_core.ConfiguredSources
import io.github.screwys.rufin.shared.rufin_core.SourceOperation
import io.github.screwys.rufin.shared.rufin_core.SourceOwner
import io.github.screwys.rufin.shared.rufin_core.sourceOperationText
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
    internal var favoriteIntents: FavoriteIntents? = null
        private set
    internal var libraryEvents: AndroidLibraryEvents? = null
        private set
    internal var favoriteRevision by mutableLongStateOf(0)
        private set
    private val settlements = MutableSharedFlow<FavoriteSettlement>(extraBufferCapacity = 32)
    internal val favoriteSettlements = settlements.asSharedFlow()
    internal fun projectedFavorite(kind: String, uri: String, fallback: Boolean): Boolean {
        favoriteRevision
        return favoriteIntents?.projectedItemFavorite(favoriteTarget(kind, uri), fallback) ?: fallback
    }
    internal fun setFavoriteIntent(kind: String, uri: String, value: Boolean) {
        favoriteIntents?.setPending(favoriteTarget(kind, uri), value)
        favoriteRevision++
    }
    internal fun releaseFavoriteIntent(kind: String, uri: String, requested: Boolean) {
        favoriteIntents?.responseMatchesPending(favoriteTarget(kind, uri), requested)
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
    var catalogRevision by mutableStateOf<CatalogRevision?>(null)
        private set
    var sourceOperation by mutableStateOf<SourceOperation>(SourceOperation.Idle)
        private set
    internal var sourceOwner by mutableStateOf<SourceOwner?>(null)
        private set
    val sourceBusy get() = sourceOperation !is SourceOperation.Idle && sourceOperation !is SourceOperation.Failed
    val sourceStatus get() = if (sourceBusy) sourceOperationText(sourceOperation) else null
    val selectedSourceName get() = sources?.sources?.firstOrNull { it.id == sources?.selectedSourceId }?.name.orEmpty()
    var sources by mutableStateOf<ConfiguredSources?>(null)
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
            sourceOperation = SourceOperation.Idle
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
                sourceOperation = SourceOperation.Idle
                sources = null
                debugLogging = false
                startupError = result?.exceptionOrNull()?.userMessage()
                val core = result?.getOrNull() ?: return@collectLatest
                val events = core.libraryEvents()
                libraryEvents = events
                favoriteIntents = events.favoriteIntents()
                val owner = core.source()
                sourceOwner = owner
                val sourceChanges = owner.subscribe()
                val operations = owner.subscribeOperation()
                val catalog = owner.subscribeCatalog()
                debugLogging = core.debugLogging()
                try {
                    sources = withContext(Dispatchers.Default) { sourceChanges.next() }
                    sourceOperation = withContext(Dispatchers.Default) { operations.next() }
                    catalogRevision = withContext(Dispatchers.Default) { catalog.next() }
                    ready = true
                    coroutineScope {
                        launch { connected.playbackNotices.collect { showFeedback(it) } }
                        launch {
                            while (isActive) {
                                val event = withContext(Dispatchers.Default) { events.next() }
                                favoriteRevision++
                                event.favorite?.let { settlements.emit(it) }
                                event.notice?.let { showFeedback(it) }
                            }
                        }
                        launch {
                            while (isActive) {
                                sources = withContext(Dispatchers.Default) { sourceChanges.next() }
                            }
                        }
                        launch {
                            while (isActive) {
                                catalogRevision = withContext(Dispatchers.Default) { catalog.next() }
                            }
                        }
                        while (isActive) {
                            val operation = withContext(Dispatchers.Default) { operations.next() }
                            if (operation is SourceOperation.Failed && operation != sourceOperation) showFeedback(operation.message)
                            sourceOperation = operation
                        }
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    showFeedback(error)
                } finally {
                    ready = false
                    sourceOperation = SourceOperation.Idle
                    sources = null
                    operations.destroy()
                    catalog.destroy()
                    sourceChanges.destroy()
                    sourceOwner = null
                    owner.destroy()
                    favoriteIntents?.destroy()
                    favoriteIntents = null
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

    fun addFolder(uri: Uri, flags: Int) = runAction {
        retainGrant(uri, flags)
        connectedService().runtime.value!!.getOrThrow().addDocumentRoot(uri.toString())
    }

    fun addMediaLibrary() = runAction { connectedService().addMediaLibrary() }

    suspend fun addMediaLibraryForSetup(): Boolean = connectedService().addMediaLibrary()

    suspend fun addFolderForSetup(uri: Uri) {
        getApplication<Application>().contentResolver.takePersistableUriPermission(uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        val service = connectedService()
        service.runtime.value!!.getOrThrow().addDocumentRoot(uri.toString())
    }

    private fun retainGrant(uri: Uri, flags: Int) {
        if (flags and Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION != 0) {
            getApplication<Application>().contentResolver.takePersistableUriPermission(uri,
                flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION))
        }
    }

    fun requestLocalNetwork() = runAction { connectedService().requestLocalNetwork() }
    internal fun openOverview(mediaUri: String, title: String, kind: String, period: String) = runAction {
        val library = browse.library ?: return@runAction
        val route = when (kind) {
            "album" -> Route.AlbumDetail(mediaUri)
            "artist" -> Route.ArtistDetail(mediaUri)
            else -> null
        }
        if (route != null) {
            sourceIdForUri(mediaUri)?.takeIf { it != sources?.selectedSourceId }?.let { sourceOwner?.selectSource(it) }
            browse.openRoute(route, title, kind)
        } else connectedService().playRequest {
            library.playRows("overview:$period:$mediaUri", listOf(mediaUri), 0UL, "now", false, translate("Listening overview"))
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
        sources?.selectedSourceId?.let { sourceOwner?.refreshSource(it, false) }
    }

    fun selectSource(id: String) = runAction {
        sourceOwner?.selectSource(id)
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
