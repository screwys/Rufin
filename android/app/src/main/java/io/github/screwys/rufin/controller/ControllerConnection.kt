package io.github.screwys.rufin.controller

import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.shared.web.Controller
import io.github.screwys.rufin.shared.web.ControllerStatus
import io.github.screwys.rufin.shared.web.controllerAddresses
import io.github.screwys.rufin.shared.rufin_core.ControllerSettings
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.isActive

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class ControllerConnection(private val model: RufinConnection) {
    var state by mutableStateOf<ControllerStatus?>(null)
        private set
    var links by mutableStateOf<List<String>>(emptyList())
        private set
    var addressError by mutableStateOf<String?>(null)
        private set
    private var bridge: Controller? = null

    suspend fun observe() {
        snapshotFlow { model.ready to model.service }.collectLatest { (ready, service) ->
            state = null; links = emptyList(); addressError = null
            if (!ready || service == null) return@collectLatest
            val owner = service.runtime.value?.getOrNull()?.controller() ?: return@collectLatest
            val changes = owner.subscribe()
            bridge = owner
            try {
                while (currentCoroutineContext().isActive) {
                    val next = changes.next()
                    val previousError = state?.error ?: addressError
                    addressError = null
                    links = try {
                        withContext(Dispatchers.IO) { controllerAddresses(next).map { "http://$it/#token=${next.token}" } }
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (failure: Exception) { addressError = failure.message; emptyList() }
                    (next.error ?: addressError)?.takeIf { it != previousError }?.let { model.showFeedback(it) }
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

    private suspend fun withSettings(action: (io.github.screwys.rufin.shared.rufin_core.SettingsOwner) -> Unit) = withContext(Dispatchers.IO) {
        model.connectedService().runtime.value!!.getOrThrow().settings().use(action)
    }

    fun enable(enabled: Boolean) = action {
        val permissionDenied = enabled && state?.settings?.remote == true && !model.connectedService().requestLocalNetworkAccess()
        withSettings { it.setControllerEnabled(enabled) }
        model.showFeedback(translate("Saved"))
        if (permissionDenied) error(translate("Local network permission is required for remote connections"))
    }

    fun remote(remote: Boolean) = action {
        if (remote && !model.connectedService().requestLocalNetworkAccess()) {
            error(translate("Local network permission is required for remote connections"))
        }
        withSettings { it.setControllerRemote(remote) }
        model.showFeedback(translate("Saved"))
    }

    fun port(port: UShort) = action("Saved") { withSettings { it.setControllerPort(port) } }
    fun copyToken(copy: (String) -> Unit) = action { bridge?.token(false)?.let(copy) }
    fun regenerateToken() = action { bridge?.token(true)?.let { model.showFeedback(translate("Saved")) } }
}

internal val ControllerSettings.remote get() = !address.startsWith("127.") && address != "::1"
