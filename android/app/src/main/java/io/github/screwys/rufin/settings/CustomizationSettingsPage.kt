package io.github.screwys.rufin.settings

import androidx.compose.foundation.shape.RoundedCornerShape

import androidx.compose.foundation.verticalScroll

import androidx.compose.foundation.rememberScrollState

import io.github.screwys.rufin.ui.SegmentedPill


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

/** A setting that takes one of `options`. A few short ones are a pill select in the row; more, or longer
 *  ones, open a sheet of rows with the picked one highlighted, as GTK marks it. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun <T> PreferenceChoice(title: String, selected: T, options: List<Pair<T, String>>,
    preview: (@Composable (T) -> Unit)? = null, enabled: (T) -> Boolean = { true }, select: (T) -> Unit,
) {
    val active = options.any { enabled(it.first) }
    if (preview == null && options.size in 2..3 && options.all { it.second.length <= 12 && enabled(it.first) == active }) {
        Surface(Modifier.fillMaxWidth(), color = MaterialTheme.colorScheme.surfaceContainer, shape = RoundedCornerShape(5.dp)) {
            Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(title, style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = if (active) 1f else .38f))
                SegmentedPill(options.map { it.first }, selected, select, Modifier.fillMaxWidth(), equal = true, enabled = active) { id, _ ->
                    Text(options.first { it.first == id }.second, style = MaterialTheme.typography.titleSmall, maxLines = 1)
                }
            }
        }
        return
    }
    var open by remember { mutableStateOf(false) }
    SettingsRow(title, summary = options.find { it.first == selected }?.second ?: selected.toString(),
        leading = preview?.let { { it(selected) } },
        trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) },
        modifier = Modifier.clickable(enabled = active) { open = true })
    if (open) ModalBottomSheet({ open = false }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp)) {
            SettingsGroup(title) {
                options.forEach { (id, name) ->
                    val picked = id == selected
                    SettingsRow(name, selected = picked, leading = preview?.let { { it(id) } }, modifier = Modifier.selectable(picked, enabled = enabled(id), role = Role.RadioButton) { select(id); open = false })
                }
            }
        }
    }
}
