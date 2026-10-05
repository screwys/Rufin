package io.github.screwys.rufin

import io.github.screwys.rufin.platform.AndroidDocuments
import io.github.screwys.rufin.platform.AndroidPermissions

import android.content.Context
import android.content.ContextWrapper
import android.system.Os
import java.io.File
import org.freedesktop.gstreamer.GStreamer

internal object NativeHost {
    private var initialized = false
    private var gstreamerInitialized = false
    init {
        System.loadLibrary("gstreamer_android")
        System.loadLibrary("rufin_android")
    }

    external fun initialize(context: Context, documents: AndroidDocuments)
    private external fun configureGstreamer()

    @Synchronized
    fun startGStreamer(context: Context, process: String = "playback") {
        if (!gstreamerInitialized) {
            // Shared file operations stage working copies in this process's temp directory.
            Os.setenv("TMPDIR", context.cacheDir.absolutePath, true)
            // Each process owns the SDK's certificate copy and registry files.
            val files = File(context.filesDir, "gstreamer/$process")
            val cache = File(context.cacheDir, "gstreamer/$process")
            check(files.mkdirs() || files.isDirectory) { "Could not create GStreamer files directory" }
            check(cache.mkdirs() || cache.isDirectory) { "Could not create GStreamer cache directory" }
            GStreamer.init(object : ContextWrapper(context.applicationContext) {
                override fun getFilesDir(): File = files
                override fun getCacheDir(): File = cache
            })
            configureGstreamer()
            gstreamerInitialized = true
        }
    }

    @Synchronized
    fun start(context: Context) {
        if (!initialized) {
            startGStreamer(context)
            initialize(context.applicationContext,
                AndroidDocuments(context.applicationContext, AndroidPermissions::request,
                    AndroidPermissions::request))
            initialized = true
        }
    }
}
