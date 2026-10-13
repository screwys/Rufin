package io.github.screwys.rufin.settings

import io.github.screwys.rufin.shared.library.HomeBlockKind

import androidx.compose.runtime.*
import io.github.screwys.rufin.core.translate

@Composable
internal fun HomeSettingsPage(preferences: PreferencesConnection, embedded: Boolean = false) {
    val names = listOf("SHOWCASE" to translate("Showcase"), "EXPLORE" to translate("Explore"), "MOST_PLAYED" to translate("Most played"),
        "NEWLY_ADDED" to translate("Newly added"), "RECENTLY_PLAYED" to translate("Recently played"), "RECENTLY_RELEASED" to translate("Recently released"), "GENRES" to translate("Genres"))
    val enabled = preferences.applicationSettings?.homeBlocks.orEmpty().map { it.name }
    val order = enabled + names.map { it.first }.filter { it !in enabled }
    SettingsPage(embedded = embedded) {
        SettingsGroup(translate("Home Blocks")) {
            SettingsReorderRows(order.map { id -> SettingsReorderItem(id, names.first { it.first == id }.second, id in enabled, id in enabled) },
                onReorder = { next, _, _, _ -> preferences.changePreference { setHomeBlocks(next.filter { it in enabled }.map(HomeBlockKind::valueOf)) } },
                onToggle = { id, on -> preferences.changePreference { setHomeBlocks((if (on) enabled + id else enabled - id).map(HomeBlockKind::valueOf)) } })
        }
    }
}
