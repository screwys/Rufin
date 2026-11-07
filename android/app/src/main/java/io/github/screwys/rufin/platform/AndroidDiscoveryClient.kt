package io.github.screwys.rufin.platform

import io.github.screwys.rufin.IDiscoveryService
import io.github.screwys.rufin.AndroidDiscoveryService

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.ParcelFileDescriptor
import java.io.IOException
import java.util.concurrent.CountDownLatch

internal class AndroidDiscoveryClient(context: Context, private val timeoutSeconds: Long) : AutoCloseable {
    private val context = context.applicationContext
    private val ready = CountDownLatch(1)
    @Volatile private var binder: IDiscoveryService? = null
    @Volatile private var failure: IOException? = null
    private var bound = false
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, service: IBinder) {
            binder = IDiscoveryService.Stub.asInterface(service)
            ready.countDown()
        }
        override fun onServiceDisconnected(name: ComponentName) {
            binder = null
            failure = IOException("Android discovery service exited")
            ready.countDown()
        }
        override fun onBindingDied(name: ComponentName) = onServiceDisconnected(name)
        override fun onNullBinding(name: ComponentName) = onServiceDisconnected(name)
    }
    private val session = lazy {
        ready.await()
        failure?.let { throw it }
        val service = binder ?: throw IOException("Android discovery service is unavailable")
        requireNotNull(service.open(timeoutSeconds))
    }

    init {
        bound = this.context.bindService(Intent(this.context, AndroidDiscoveryService::class.java),
            connection, Context.BIND_AUTO_CREATE)
        if (!bound) throw IOException("Could not bind Android discovery service")
    }

    fun request(request: String): ByteArray {
        failure?.let { throw it }
        val service = session.value
        val pipe = ParcelFileDescriptor.createPipe()
        try {
            service.discover(request, pipe[1])
            pipe[1].close()
            return ParcelFileDescriptor.AutoCloseInputStream(pipe[0]).use { it.readBytes() }
        } finally {
            pipe[0].close()
            pipe[1].close()
        }
    }

    @Synchronized
    override fun close() {
        if (bound) {
            bound = false
            try {
                if (session.isInitialized()) session.value.close()
            } finally { context.unbindService(connection) }
        }
    }
}
