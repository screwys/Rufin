package io.github.screwys.rufin.controller

import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidController
import io.github.screwys.rufin.core.AndroidControllerState
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.isActive

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class ControllerConnection(private val model: RufinConnection) {
    var state by mutableStateOf<AndroidControllerState?>(null)
        private set
    private var bridge: AndroidController? = null

    suspend fun observe() {
        snapshotFlow { model.ready to model.service }.collectLatest { (ready, service) ->
            state = null
            if (!ready || service == null) return@collectLatest
            val owner = service.runtime.value?.getOrNull()?.controller() ?: return@collectLatest
            val changes = owner.subscribe()
            bridge = owner
            try {
                while (currentCoroutineContext().isActive) {
                    val next = changes.next()
                    next.error?.takeIf { it != state?.error }?.let { model.showFeedback(it) }
                    state = next
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { action { throw error } }
            finally { bridge = null; changes.destroy(); owner.destroy() }
        }
    }

    fun action(message: String? = null, action: suspend () -> Unit) = model.runAction {
        try {
            action()
            message?.let { model.showFeedback(translate(it)) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) {
            error.message?.let { model.showFeedback(it) }
            throw error
        }
    }

    fun enable(enabled: Boolean) = action {
        val permissionDenied = enabled && state?.remote == true && !model.connectedService().requestLocalNetworkAccess()
        bridge?.setEnabled(enabled)?.let { model.showFeedback(translate("Saved")) }
        if (permissionDenied) error(translate("Local network permission is required for remote connections"))
    }

    fun remote(remote: Boolean) = action {
        if (remote && !model.connectedService().requestLocalNetworkAccess()) {
            error(translate("Local network permission is required for remote connections"))
        }
        bridge?.setRemote(remote)?.let { model.showFeedback(translate("Saved")) }
    }

    fun port(port: UShort) = action { bridge?.setPort(port)?.let { model.showFeedback(translate("Saved")) } }
    fun copyToken(copy: (String) -> Unit) = action { bridge?.token()?.let(copy) }
    fun regenerateToken() = action { bridge?.regenerateToken()?.let { model.showFeedback(translate("Saved")) } }
}
