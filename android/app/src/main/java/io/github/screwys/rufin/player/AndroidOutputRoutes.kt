package io.github.screwys.rufin.player

import android.content.Context
import android.content.Intent
import android.provider.Settings
import androidx.mediarouter.app.SystemOutputSwitcherDialogController
import androidx.mediarouter.media.MediaRouteSelector
import androidx.mediarouter.media.MediaRouter
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow

data class AndroidOutputRoute(
    val id: String,
    val name: String,
    val deviceType: Int,
    val selected: Boolean,
    val enabled: Boolean,
)

/** Android owns device routing; Rufin's Cast and Connect outputs keep their own owners. */
class AndroidOutputRoutes(context: Context) : AutoCloseable {
    private val router = MediaRouter.getInstance(context.applicationContext)
    private val mutableRoutes = MutableStateFlow<List<AndroidOutputRoute>>(emptyList())
    val routes = mutableRoutes.asStateFlow()

    private val callback = object : MediaRouter.Callback() {
        override fun onRouteAdded(router: MediaRouter, route: MediaRouter.RouteInfo) = refresh()
        override fun onRouteRemoved(router: MediaRouter, route: MediaRouter.RouteInfo) = refresh()
        override fun onRouteChanged(router: MediaRouter, route: MediaRouter.RouteInfo) = refresh()
        override fun onRouteSelected(router: MediaRouter, route: MediaRouter.RouteInfo, reason: Int) = refresh()
        override fun onRouteUnselected(router: MediaRouter, route: MediaRouter.RouteInfo, reason: Int) = refresh()
    }

    init {
        router.addCallback(MediaRouteSelector.EMPTY, callback, MediaRouter.CALLBACK_FLAG_UNFILTERED_EVENTS)
        refresh()
    }

    private fun refresh() {
        mutableRoutes.value = router.routes.filter { it.isSystemRoute }.map {
            AndroidOutputRoute(it.id, it.name, it.deviceType, it.isSelected, it.isEnabled)
        }
    }

    fun select(id: String) {
        router.selectRoute(router.routes.first { it.id == id && it.isSystemRoute })
    }

    // The system acts on the user's selection. This does not scan or connect Bluetooth devices itself.
    fun showSystemDialog(context: Context): Boolean = SystemOutputSwitcherDialogController.showDialog(context)

    fun openBluetoothSettings(context: Context) {
        context.startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
    }

    override fun close() = router.removeCallback(callback)
}
