package io.github.screwys.rufin.connect

import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import io.github.screwys.rufin.ui.userMessage

internal data class ConnectConfirmation(val kind: AndroidConnectConfirmation, val message: String, val retry: () -> Unit)

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class ConnectConnection(private val model: RufinConnection) {
    var pageVisible by mutableStateOf(false)
    var setupActive by mutableStateOf(false)
        private set
    var state by mutableStateOf<AndroidConnectStatus?>(null)
        private set
    var pending by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    var confirmation by mutableStateOf<ConnectConfirmation?>(null)
        private set
    var fileSources by mutableStateOf<List<AndroidConnectFileSource>>(emptyList())
        private set
    var musicFolders by mutableStateOf<List<AndroidConnectMusicFolder>>(emptyList())
        private set
    private var bridge: AndroidConnect? = null
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
                val subscription = owner.subscribe()
                try {
                    fileSources = withContext(Dispatchers.IO) { owner.fileSources() }
                    coroutineScope {
                        launch {
                            snapshotFlow { model.sources }.collect {
                                fileSources = withContext(Dispatchers.IO) { owner.fileSources() }
                            }
                        }
                        while (isActive) {
                            val next = subscription.next()
                            if (next.connecting) setupActive = true
                            if (!next.connecting && !next.setupPending && !next.adopting) setupActive = false
                            state = next
                        }
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: Exception) { error = failure.userMessage() }
                finally {
                    bridge = null
                    state = null
                    setupActive = false
                    subscription.destroy()
                    owner.destroy()
                }
            }
        }
    }

    private fun request(operation: suspend (AndroidConnect) -> AndroidConnectStatus,
        confirmed: (suspend (AndroidConnect) -> AndroidConnectStatus)? = null, success: String? = null) {
        val owner = bridge ?: return
        model.runAction {
            pending = true
            error = null
            dismissedStatusError = null
            try { operation(owner).let { if (bridge === owner) { state = it; success?.let { message -> model.showFeedback(message) } } } }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: AndroidConnectException.ConfirmationRequired) {
                if (confirmed != null) confirmation = ConnectConfirmation(failure.confirmation, failure.reason) { request(confirmed, success = success) }
                else error = failure.reason
            }
            catch (failure: AndroidConnectException.Failed) { error = failure.reason }
            catch (failure: Exception) { error = failure.userMessage() }
            finally { pending = false }
        }
    }

    fun action(action: AndroidConnectAction) {
        val needsLocalNetwork = when (action) {
            AndroidConnectAction.Discover -> true
            is AndroidConnectAction.Invite -> state?.nearby == true
            is AndroidConnectAction.Network -> action.nearby && state?.nearby != true
            else -> false
        }
        val confirmed: (suspend (AndroidConnect) -> AndroidConnectStatus)? =
            when {
                action is AndroidConnectAction.Pair && action.approve && !action.replace ->
                    { owner -> owner.execute(action.copy(replace = true)) }
                else -> null
            }
        request({ owner ->
            if (needsLocalNetwork && !model.connectedService().requestLocalNetworkAccess()) {
                throw IllegalStateException(translate("Local network permission was denied"))
            }
            owner.execute(action)
        }, confirmed = confirmed, success = if (action is AndroidConnectAction.ExportRemote) translate("Saved") else null)
    }
    fun join(invitation: String) = request({ it.execute(AndroidConnectAction.Join(invitation, false)) },
        { it.execute(AndroidConnectAction.Join(invitation, true)) })
    fun refreshMusicFolders() = model.runAction { bridge?.let { musicFolders = it.musicFolders() } }
    fun folder(folder: AndroidConnectMusicFolder, uri: String?) = request({ owner ->
        val updated = owner.execute(AndroidConnectAction.Folder(folder.sourceId, folder.rootId, uri))
        musicFolders = owner.musicFolders()
        updated
    })
    fun encoding(encoding: AndroidConnectEncoding) = request({ it.execute(AndroidConnectAction.Encoding(encoding, false)) },
        { it.execute(AndroidConnectAction.Encoding(encoding, true)) })
    fun importDocument(uri: String) = request({ it.importDocument(uri, false) },
        { it.importDocument(uri, true) }, success = translate("Applied"))
    fun exportDocument(uri: String) = request({ it.exportDocument(uri) }, success = translate("Saved"))
    fun importRemote(source: String, path: String) = request({
        it.execute(AndroidConnectAction.ImportRemote(source, path, false))
    }, { it.execute(AndroidConnectAction.ImportRemote(source, path, true)) }, success = translate("Applied"))
    fun exportRemote(source: String, path: String) = action(AndroidConnectAction.ExportRemote(source, path))
    suspend fun profileFiles(source: String, folder: String): List<String> = bridge?.profileFiles(source, folder).orEmpty()
    fun confirm() { confirmation?.let { choice -> confirmation = null; choice.retry() } }
    fun cancelConfirmation() { confirmation = null }
    fun reportError(failure: Exception) { error = failure.userMessage() }
    fun dismissError() { error = null; dismissedStatusError = state?.error }
}
