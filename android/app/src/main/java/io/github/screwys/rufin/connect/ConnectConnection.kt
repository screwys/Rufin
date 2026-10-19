package io.github.screwys.rufin.connect

import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Action as ConnectAction
import io.github.screwys.rufin.shared.rufin_core.Status as ConnectStatus
import io.github.screwys.rufin.shared.rufin_core.Control as ConnectControl
import io.github.screwys.rufin.shared.downloads.DownloadDirectory

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import io.github.screwys.rufin.ui.userMessage

internal data class ConnectMusicFolder(val sourceId: String, val rootId: String, val title: String, val location: String?)

internal data class ConnectConfirmation(val kind: Confirmation, val message: String, val retry: () -> Unit)

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class ConnectConnection(private val model: RufinConnection) {
    var pageVisible by mutableStateOf(false)
    var operationActive by mutableStateOf(false)
        private set
    var state by mutableStateOf<ConnectStatus?>(null)
        private set
    var pending by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    var confirmation by mutableStateOf<ConnectConfirmation?>(null)
        private set
    var fileSources by mutableStateOf<List<FileIntegration>>(emptyList())
        private set
    var musicFolders by mutableStateOf<List<ConnectMusicFolder>>(emptyList())
        private set
    private var bridge: ConnectOwner? = null
    private var dismissedStatusError: String? = null
    val visibleError get() = error ?: state?.error?.takeUnless { it == dismissedStatusError }

    suspend fun observe() {
        snapshotFlow { model.service }.collectLatest { service ->
            state = null
            if (service == null) return@collectLatest
            service.runtime.collectLatest { result ->
                val runtime = result?.getOrNull() ?: return@collectLatest
                val owner = runtime.connect()
                bridge = owner
                val subscription = owner.changes()
                val source = runtime.source()
                try {
                    fileSources = withContext(Dispatchers.IO) { source.fileIntegrations() }
                    coroutineScope {
                        launch {
                            snapshotFlow { model.sources }.collect {
                                fileSources = withContext(Dispatchers.IO) { source.fileIntegrations() }
                            }
                        }
                        while (isActive) {
                            val next = withContext(Dispatchers.Default) { subscription.next() }
                            if (next.connecting && state?.connecting != true) operationActive = true
                            if (!next.connecting && next.error == null) operationActive = false
                            state = next
                        }
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: Exception) { error = failure.userMessage() }
                finally {
                    bridge = null
                    state = null
                    operationActive = false
                    subscription.destroy()
                    source.destroy()
                    owner.destroy()
                }
            }
        }
    }

    private fun request(operation: suspend (ConnectOwner) -> ConnectStatus,
        confirmed: (suspend (ConnectOwner) -> ConnectStatus)? = null, success: String? = null, blocking: Boolean = true) {
        val owner = bridge ?: return
        model.runAction {
            if (blocking) pending = true
            error = null
            dismissedStatusError = null
            try { operation(owner).let { if (bridge === owner) { state = it; success?.let { message -> model.showFeedback(message) } } } }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: ActionException.ConfirmationRequired) {
                if (confirmed != null) confirmation = ConnectConfirmation(failure.confirmation, failure.reason) { request(confirmed, success = success) }
                else error = failure.reason
            }
            catch (failure: ActionException.Failed) { error = failure.v1 }
            catch (failure: Exception) { error = failure.userMessage() }
            finally { if (blocking) pending = false }
        }
    }

    fun action(action: ConnectAction) {
        val needsLocalNetwork = when (action) {
            ConnectAction.Discover -> true
            is ConnectAction.Invite -> state?.settings?.nearby == true
            is ConnectAction.Network -> action.nearby && state?.settings?.nearby != true
            else -> false
        }
        val confirmed: (suspend (ConnectOwner) -> ConnectStatus)? =
            when {
                action is ConnectAction.Pair && action.approve && !action.replace ->
                    { owner -> owner.execute(action.copy(replace = true)) }
                else -> null
            }
        request({ owner ->
            if (needsLocalNetwork && !model.connectedService().requestLocalNetworkAccess()) {
                throw IllegalStateException(translate("Local network permission was denied"))
            }
            owner.execute(action)
        }, confirmed = confirmed, success = if (action is ConnectAction.Export) translate("Saved") else null,
            blocking = action != ConnectAction.Refresh)
    }
    fun join(invitation: String) = request({ it.execute(ConnectAction.Join(invitation, false)) },
        { it.execute(ConnectAction.Join(invitation, true)) })
    private suspend fun loadMusicFolders(owner: ConnectOwner): List<ConnectMusicFolder> {
        val settings = owner.status().settings
        return model.sources?.sources.orEmpty().flatMap { source ->
            owner.roots(source.id).map { root ->
                val location = settings.folders["${source.id}/${root.id}"] ?: settings.folders[source.id]
                ConnectMusicFolder(source.id, root.id, "${source.name} · ${root.label}", when (location) {
                    is DownloadDirectory.Native -> location.v1
                    is DownloadDirectory.Document -> location.uri
                    null -> null
                })
            }
        }
    }
    fun refreshMusicFolders() = model.runAction { bridge?.let { musicFolders = loadMusicFolders(it) } }
    fun folder(folder: ConnectMusicFolder, uri: String?) = request({ owner ->
        val updated = owner.execute(ConnectAction.Folder(folder.sourceId, folder.rootId, uri?.let { DownloadDirectory.Document(it) } ?: DownloadDirectory.Native("")))
        musicFolders = loadMusicFolders(owner)
        updated
    })
    fun importDocument(uri: String) = request({ it.importDocument(uri, false, null) },
        { it.importDocument(uri, true, null) }, success = translate("Applied"))
    fun exportDocument(uri: String) = request({ it.exportDocument(uri) }, success = translate("Saved"))
    fun importRemote(source: String, path: String) = request({
        it.execute(ConnectAction.Import(path, source, false, null))
    }, { it.execute(ConnectAction.Import(path, source, true, null)) }, success = translate("Applied"))
    fun exportRemote(source: String, path: String) = action(ConnectAction.Export(path, source))
    suspend fun profileFiles(source: String, folder: String): List<String> = bridge?.listProfileFiles(source, folder).orEmpty()
    fun confirm() { confirmation?.let { choice -> confirmation = null; choice.retry() } }
    fun cancelConfirmation() { confirmation = null }
    fun dismissOperation() { operationActive = false }
    fun showOperation() { operationActive = true }
    fun reportError(failure: Exception) { error = failure.userMessage() }
    fun dismissError() { error = null; dismissedStatusError = state?.error }
}
