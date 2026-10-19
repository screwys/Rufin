package io.github.screwys.rufin.settings


import androidx.compose.foundation.layout.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.browse.BrowseSwipeAction
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerLayoutPreferences

@Composable
internal fun CustomizationSettingsPage(preferences: PreferencesConnection, browse: BrowseConnection, player: PlayerConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings(); player.refreshSettings() }
    SettingsPage {
        preferences.state?.let {
            SettingsGroup(translate("Accessibility")) {
                SettingsSwitch(translate("Reduce motion"), preferences.reduceMotion) { preferences.changePreference { setReduceMotion(it) } }
            }
            PlayerLayoutPreferences(player)
            SettingsGroup(translate("Gestures")) {
                val swipeActions = BrowseSwipeAction.entries.map { it.name to it.label }
                PreferenceChoice(translate("Swipe left"), preferences.swipeLeft.name, swipeActions,
                    preview = { id -> BrowseSwipeAction.entries.find { it.name == id }?.let { RufinIcon(it.icon, null, Modifier.size(24.dp)) } },
                    select = { preferences.selectSwipeAction(true, it) })
                PreferenceChoice(translate("Swipe right"), preferences.swipeRight.name, swipeActions,
                    preview = { id -> BrowseSwipeAction.entries.find { it.name == id }?.let { RufinIcon(it.icon, null, Modifier.size(24.dp)) } },
                    select = { preferences.selectSwipeAction(false, it) })
            }
            SettingsExpander(translate("Sidebar Items")) {
                RouteSettingsPage(browse, embedded = true)
            }
            SettingsExpander(translate("Home Blocks")) {
                HomeSettingsPage(preferences, embedded = true)
            }
        } ?: Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
    }
}

@Composable
internal fun PreferenceChoice(title: String, selected: String, options: List<Pair<String, String>>,
    preview: (@Composable (String) -> Unit)? = null, enabled: (String) -> Boolean = { true },
    trailingAction: (@Composable () -> Unit)? = null, select: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    SettingsRow(
        title,
        summary = options.find { it.first == selected }?.second ?: selected,
        leading = preview?.let { { it(selected) } },
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                trailingAction?.invoke()
                RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp))
            }
        },
        modifier = Modifier.clickable { expanded = !expanded },
    )
    if (expanded) AlertDialog(onDismissRequest = { expanded = false }, title = { Text(title) }, text = {
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
            items(options, key = { it.first }) { (id, name) ->
                ListItem(headlineContent = { Text(name) },
                    leadingContent = preview?.let { { it(id) } },
                    trailingContent = { RadioButton(selected == id, null) },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    modifier = Modifier.selectable(selected == id, enabled = enabled(id), role = Role.RadioButton) { select(id); expanded = false })
            }
        }
    }, confirmButton = { TextButton({ expanded = false }) { Text(translate("Cancel")) } })
}
