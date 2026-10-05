package io.github.screwys.rufin.settings

import androidx.compose.runtime.*
import io.github.screwys.rufin.core.translate
import org.json.JSONArray

@Composable
internal fun HomeSettingsPage(preferences: PreferencesConnection, embedded: Boolean = false) {
    val names = listOf("Showcase" to translate("Showcase"), "Explore" to translate("Explore"), "MostPlayed" to translate("Most played"),
        "NewlyAdded" to translate("Newly added"), "RecentlyPlayed" to translate("Recently played"), "RecentlyReleased" to translate("Recently released"), "Genres" to translate("Genres"))
    val array = preferences.applicationSettings?.optJSONArray("home_blocks") ?: JSONArray()
    val enabled = (0 until array.length()).map { array.getString(it) }
    val order = enabled + names.map { it.first }.filter { it !in enabled }
    SettingsPage(embedded = embedded) {
        SettingsGroup(translate("Home Blocks")) {
            SettingsReorderRows(order.map { id -> SettingsReorderItem(id, names.first { it.first == id }.second, id in enabled, id in enabled) },
                onReorder = { next, _, _, _ -> preferences.changePreference("home_blocks", JSONArray(next.filter { it in enabled })) },
                onToggle = { id, on -> preferences.changePreference("home_blocks", JSONArray(if (on) enabled + id else enabled - id)) })
        }
    }
}
