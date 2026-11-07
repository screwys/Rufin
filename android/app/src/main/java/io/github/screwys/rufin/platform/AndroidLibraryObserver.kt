package io.github.screwys.rufin.platform

import android.content.Context
import android.database.ContentObserver
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.util.Log

internal class AndroidLibraryObserver(context: Context, private val changed: () -> Unit) : AutoCloseable {
    private val resolver = context.contentResolver
    private var roots = emptyList<String>()
    private val observer = object : ContentObserver(Handler(Looper.getMainLooper())) {
        override fun onChange(selfChange: Boolean) { changed() }
    }

    fun update(uris: List<String>) {
        if (uris == roots) return
        resolver.unregisterContentObserver(observer)
        roots = uris
        uris.forEach { uri ->
            try { resolver.registerContentObserver(Uri.parse(uri), true, observer) }
            catch (error: SecurityException) {
                // Some document grants permit reads without permitting a change subscription.
                Log.d("Rufin", "Document provider did not allow change observation", error)
            }
        }
    }

    override fun close() = resolver.unregisterContentObserver(observer)
}
