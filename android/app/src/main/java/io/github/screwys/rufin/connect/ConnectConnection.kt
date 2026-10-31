package io.github.screwys.rufin.connect

import android.content.Intent
import android.net.Uri
import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.rufin_core.Action as ConnectAction
import io.github.screwys.rufin.shared.rufin_core.Status as ConnectStatus
import io.github.screwys.rufin.shared.rufin_core.Control as ConnectControl

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import io.github.screwys.rufin.ui.userMessage

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
    private var bridge: ConnectOwner? = null
    private var dismissedStatusError: String? = null
    private var offered: Pair<String, Long>? = null
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
                        // The document tree the sync folder uses; its grant goes once Connect stops
                        // using it: the folder changed or cleared, Connect turned off, or this
                        // device left or was removed. The Local source may still read that tree.
                        var granted: String? = null
                        while (isActive) {
                            val next = withContext(Dispatchers.Default) { subscription.next() }
                            val document = (next.settings.folder as? SyncFolder.Document)?.document
                            if (granted != null && granted != document && !runtime.hasDocumentRoot(granted)) {
                                service.contentResolver.releasePersistableUriPermission(Uri.parse(granted),
                                    Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                            }
                            granted = document
                            if (next.connecting && state?.connecting != true) operationActive = true
                            if (!next.connecting && next.error == null) operationActive = false
                            val offer = next.continuation?.let { it.device to it.at }
                            if (offer != null && offer != offered) model.showFeedback(
                                translate("Continue from {device}").replace("{device}", next.continuation!!.name))
                            offered = offer
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
        confirmed: (suspend (ConnectOwner) -> ConnectStatus)? = null, blocking: Boolean = true) {
        val owner = bridge ?: return
        model.runAction {
            if (blocking) pending = true
            error = null
            dismissedStatusError = null
            try { operation(owner).let { if (bridge === owner) state = it } }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: ActionException.ConfirmationRequired) {
                if (confirmed != null) confirmation = ConnectConfirmation(failure.confirmation, failure.reason) { request(confirmed) }
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
        }, confirmed = confirmed, blocking = action != ConnectAction.Refresh)
    }
    fun join(invitation: String) = request({ it.execute(ConnectAction.Join(invitation, false)) },
        { it.execute(ConnectAction.Join(invitation, true)) })
    fun confirm() { confirmation?.let { choice -> confirmation = null; choice.retry() } }
    fun cancelConfirmation() { confirmation = null }
    fun dismissOperation() { operationActive = false }
    fun showOperation() { operationActive = true }
    fun reportError(failure: Exception) { error = failure.userMessage() }
    fun dismissError() { error = null; dismissedStatusError = state?.error }
}
