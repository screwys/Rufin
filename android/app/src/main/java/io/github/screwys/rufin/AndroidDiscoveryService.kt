package io.github.screwys.rufin

import android.app.Service
import android.content.Intent
import android.os.IBinder
import android.os.ParcelFileDescriptor
import io.github.screwys.rufin.core.AndroidDiscoveryWorker
import org.json.JSONObject

class AndroidDiscoveryService : Service() {
    private val binder = object : IDiscoveryService.Stub() {
        override fun open(timeoutSeconds: Long): IDiscoverySession = DiscoverySession(timeoutSeconds)
    }

    // Each Reader has its own Binder, so its one-way requests are ordered while
    // independent Readers can extract metadata concurrently on Binder threads.
    private inner class DiscoverySession(private val timeout: Long) : IDiscoverySession.Stub() {
        private var worker: AndroidDiscoveryWorker? = null

        override fun discover(request: String, pipe: ParcelFileDescriptor) {
            ParcelFileDescriptor.AutoCloseOutputStream(pipe).use { output ->
                val response = try {
                    NativeHost.startGStreamer(this@AndroidDiscoveryService, "discovery")
                    val current = worker ?: AndroidDiscoveryWorker(timeout.toULong()).also { worker = it }
                    current.request(request)
                } catch (error: Exception) {
                    (JSONObject().put("Error", error.message.orEmpty()).toString() + "\n").toByteArray()
                }
                output.write(response)
            }
        }

        override fun close() {
            worker?.destroy()
            worker = null
        }
    }

    override fun onBind(intent: Intent): IBinder = binder

}
