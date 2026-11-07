package io.github.screwys.rufin.settings

import androidx.compose.runtime.*
import io.github.screwys.rufin.core.translate

@Composable
internal fun HomeSettingsPage(preferences: PreferencesConnection, embedded: Boolean = false) {
    val blocks = preferences.applicationSettings?.homeBlocks
    val rows = remember(blocks) { if (blocks == null) emptyList() else preferences.nativeSettings().homeBlockRows() }
    val block = { id: String -> rows.first { it.block.name == id }.block }
    SettingsPage(embedded = embedded) {
        SettingsGroup(if (embedded) null else translate("Home Blocks")) {
            SettingsReorderRows(rows.map { SettingsReorderItem(it.block.name, translate(it.title), it.visible, it.visible, it.canToggle) },
                onReorder = { _, id, target, after -> preferences.changePreference { reorderHomeBlock(block(id), block(target), after) } },
                onToggle = { id, on -> preferences.changePreference { setHomeBlockVisible(block(id), on) } })
        }
    }
}
