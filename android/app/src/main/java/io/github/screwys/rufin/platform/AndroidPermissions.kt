package io.github.screwys.rufin.platform

import android.content.IntentSender
import android.os.Handler
import android.os.Looper
import java.util.ArrayDeque
import java.util.concurrent.CompletableFuture

internal object AndroidPermissions {
    sealed interface Prompt {
        data class Consent(val sender: IntentSender) : Prompt
        data class Permission(val permission: String) : Prompt
    }
    private data class Request(val prompt: Prompt, val result: CompletableFuture<Boolean>)
    private val main = Handler(Looper.getMainLooper())
    private val pending = ArrayDeque<Request>()
    private var launch: ((Prompt) -> Unit)? = null
    private var showing = false

    // Host operations run on I/O workers. Only an active Activity can ask for consent.
    fun request(sender: IntentSender) = request(Prompt.Consent(sender))
    fun request(permission: String) = request(Prompt.Permission(permission))

    private fun request(prompt: Prompt): Boolean {
        val result = CompletableFuture<Boolean>()
        main.post {
            if (launch == null) result.complete(false)
            else {
                pending.addLast(Request(prompt, result))
                showNext()
            }
        }
        return result.get()
    }

    fun attach(launcher: (Prompt) -> Unit) {
        launch = launcher
        showNext()
    }

    fun detach() { launch = null }

    fun complete(granted: Boolean) {
        if (showing) {
            showing = false
            pending.removeFirst().result.complete(granted)
        }
        showNext()
    }

    fun cancel() {
        showing = false
        while (pending.isNotEmpty()) pending.removeFirst().result.complete(false)
    }

    private fun showNext() {
        val launcher = launch ?: return
        if (showing || pending.isEmpty()) return
        showing = true
        try { launcher(pending.first.prompt) }
        catch (error: Exception) {
            showing = false
            pending.removeFirst().result.completeExceptionally(error)
            showNext()
        }
    }
}
