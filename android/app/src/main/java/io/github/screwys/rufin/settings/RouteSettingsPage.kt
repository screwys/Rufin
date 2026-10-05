package io.github.screwys.rufin.settings

import androidx.compose.runtime.*
import io.github.screwys.rufin.browse.BrowseConnection

@Composable
internal fun RouteSettingsPage(browse: BrowseConnection, embedded: Boolean = false) {
    val routes = browse.routeSettings
    SettingsPage(embedded = embedded) {
        SettingsGroup {
            SettingsReorderRows(routes.map { SettingsReorderItem(it.descriptor.id, it.descriptor.title, it.visible) },
                onReorder = { _, id, target, after -> browse.reorderRoute(id, target, after) },
                onStep = browse::moveRoute,
                onToggle = browse::setRouteVisible)
        }
    }
}
