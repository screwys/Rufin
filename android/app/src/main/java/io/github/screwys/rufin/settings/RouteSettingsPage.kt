package io.github.screwys.rufin.settings

import androidx.compose.runtime.*
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.core.translate

@Composable
internal fun RouteSettingsPage(browse: BrowseConnection) {
    val routes = browse.routeSettings
    SettingsPage {
        SettingsGroup(description = translate("Drag to reorder.")) {
            SettingsReorderRows(routes.map { SettingsReorderItem(it.descriptor.id, it.descriptor.title, it.visible) },
                onReorder = { _, id, target, after -> browse.reorderRoute(id, target, after) },
                onStep = browse::moveRoute,
                onToggle = browse::setRouteVisible)
        }
    }
}
