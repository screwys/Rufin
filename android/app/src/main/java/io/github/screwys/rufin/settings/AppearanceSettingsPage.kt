package io.github.screwys.rufin.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.browse.BrowseSwipeAction

@Composable
@OptIn(ExperimentalMaterial3Api::class)
internal fun AppearanceSettingsPage(preferences: PreferencesConnection, browse: BrowseConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val label: (String) -> String = { translate(it) }
    val systemDark = isSystemInDarkTheme()
    val importTheme = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(preferences::importTheme) }
    SettingsPage {
        preferences.state?.let { settings ->
            SettingsGroup(translate("Theme")) {
                val themes = listOf("system" to label("System"), "light" to label("Light"), "dark" to label("Dark")) +
                    settings.themes.filter { it.id != "builtin:light" && it.id != "builtin:dark" }.map { it.id to it.name }
                PreferenceChoice(label("Theme"), settings.themeId, themes, select = preferences::selectTheme,
                    trailingAction = {
                        val title = label("Import theme")
                        TooltipBox(positionProvider = TooltipDefaults.rememberTooltipPositionProvider(TooltipAnchorPosition.Above),
                            tooltip = { PlainTooltip { Text(title) } }, state = rememberTooltipState()) {
                            IconButton({ importTheme.launch(arrayOf("*/*")) }) {
                                RufinIcon("rufin-document-open-symbolic", title)
                            }
                        }
                    },
                    preview = { id ->
                        val themeId = when (id) {
                            "system" -> if (systemDark) "builtin:dark" else "builtin:light"
                            "light" -> "builtin:light"
                            "dark" -> "builtin:dark"
                            else -> id
                        }
                        val theme = settings.themes.find { it.id == themeId }
                        val accents = theme?.accents.orEmpty()
                        val colors = remember(theme?.colors) { ThemeColors(theme?.colors.orEmpty()) }
                        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                            if (accents.isEmpty()) AccentSwatch(colors.named("accent-bg-color") ?: MaterialTheme.colorScheme.primary)
                            else accents.take(3).forEach { AccentSwatch(it.color) }
                        }
                    })
                val selectedTheme = settings.themes.find { it.id == settings.themeId }
                val customAccents = selectedTheme?.accents.orEmpty()
                val options = if (customAccents.isEmpty()) settings.accents.map { it.id to label(it.id) } else customAccents.map { it.id to it.name }
                val selected = if (customAccents.isEmpty()) settings.accentId else settings.themeAccent ?: customAccents.first().id
                PreferenceChoice(label("Accent color"), selected, options,
                    select = { if (customAccents.isEmpty()) preferences.selectAccent(it) else preferences.selectThemeAccent(selectedTheme!!.id, it) },
                    preview = { id -> AccentSwatch((if (customAccents.isEmpty()) settings.accents else customAccents).find { it.id == id }?.color) })
            }
            SettingsGroup(translate("Gestures")) {
                val swipeActions = BrowseSwipeAction.entries.map { it.name to it.label }
                PreferenceChoice(translate("Swipe left"), preferences.swipeLeft.name, swipeActions,
                    preview = { id -> BrowseSwipeAction.entries.find { it.name == id }?.let { RufinIcon(it.icon, null, Modifier.size(24.dp)) } },
                    select = { preferences.selectSwipeAction(true, it) })
                PreferenceChoice(translate("Swipe right"), preferences.swipeRight.name, swipeActions,
                    preview = { id -> BrowseSwipeAction.entries.find { it.name == id }?.let { RufinIcon(it.icon, null, Modifier.size(24.dp)) } },
                    select = { preferences.selectSwipeAction(false, it) })
            }
            HomeSettingsPage(preferences, embedded = true)
            RouteSettingsPage(browse, embedded = true)
            SettingsGroup(translate("Accessibility")) {
                SettingsSwitch(translate("Reduce motion"), preferences.reduceMotion) { preferences.changePreference("reduce_motion", it) }
            }
            settings.errors.forEach { io.github.screwys.rufin.ui.ErrorNotice(it) }
            preferences.themeErrors.forEach { io.github.screwys.rufin.ui.ErrorNotice(it) }
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

@Composable
private fun AccentSwatch(value: String?) {
    val color = remember(value) { value?.let { ThemeColors(mapOf("accent" to it)).named("accent") } }
        ?: MaterialTheme.colorScheme.primary
    AccentSwatch(color)
}

@Composable
private fun AccentSwatch(color: Color) {
    Surface(Modifier.size(28.dp), RoundedCornerShape(8.dp), color = color) {}
}
