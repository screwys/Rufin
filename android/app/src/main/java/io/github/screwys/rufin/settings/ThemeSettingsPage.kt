package io.github.screwys.rufin.settings

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidAccentOption
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.RufinIcon

@Composable
internal fun ThemeSettingsPage(preferences: PreferencesConnection) {
    val settings = preferences.state
    if (settings == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val systemDark = isSystemInDarkTheme()
    val themes = listOf("system" to translate("System"), "light" to translate("Light"), "dark" to translate("Dark")) +
        settings.themes.filter { it.id != "builtin:light" && it.id != "builtin:dark" }.map { it.id to it.name }
    LazyVerticalGrid(GridCells.Adaptive(96.dp), Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp + LocalPlayerBottomPadding.current),
        horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(themes, key = { it.first }) { (id, title) ->
            val themeId = when (id) {
                "system" -> if (systemDark) "builtin:dark" else "builtin:light"
                "light" -> "builtin:light"
                "dark" -> "builtin:dark"
                else -> id
            }
            val theme = settings.themes.find { it.id == themeId }
            val selected = settings.themeId == id
            val standard = id == "system" || id == "light" || id == "dark"
            val colors = MaterialTheme.colorScheme
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Surface(Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { preferences.selectTheme(id) }
                    .semantics { contentDescription = title },
                    shape = RoundedCornerShape(12.dp), color = colors.surfaceContainer,
                    border = BorderStroke(if (selected) 2.dp else 1.dp, if (selected) colors.primary else colors.outlineVariant)) {
                    ThemePreview(theme, theme?.dark ?: systemDark,
                        standard && settings.accentId == "System",
                        Modifier.padding(5.dp).fillMaxWidth().aspectRatio(9f / 16f))
                }
                Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(title, Modifier.weight(1f).padding(start = 2.dp),
                        style = MaterialTheme.typography.labelMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    if (standard || (theme?.accents?.size ?: 0) > 1) {
                        ThemeAccentMenu(
                            options = if (standard) settings.accents else theme!!.accents,
                            selected = if (standard) settings.accentId else theme!!.accentId,
                            standard = standard,
                            dark = theme?.dark ?: systemDark,
                            select = { if (standard) preferences.selectAccent(it) else preferences.selectThemeAccent(id, it) },
                        )
                    }
                }
            }
        }
        items(settings.errors + preferences.themeErrors, span = { GridItemSpan(maxLineSpan) }) {
            io.github.screwys.rufin.ui.ErrorNotice(it)
        }
    }
}

@Composable
private fun ThemeAccentMenu(options: List<AndroidAccentOption>, selected: String?, standard: Boolean, dark: Boolean, select: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val current = options.find { it.id == selected } ?: options.first()
    val label = translate("Accent color")
    val currentName = if (standard) translate(current.id) else current.name
    Box {
        IconButton({ expanded = true }, Modifier.semantics {
            contentDescription = label
            stateDescription = currentName
        }) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                AccentSwatch(current.color, dark)
                RufinIcon("rufin-go-down-symbolic", null, Modifier.size(12.dp))
            }
        }
        DropdownMenu(expanded, { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(if (standard) translate(option.id) else option.name) },
                    leadingIcon = { AccentSwatch(option.color, dark) },
                    trailingIcon = {
                        if (option.id == current.id) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp))
                    },
                    onClick = { select(option.id); expanded = false },
                )
            }
        }
    }
}

@Composable
private fun AccentSwatch(value: String?, dark: Boolean) {
    val color = remember(value) { value?.let { ThemeColors(mapOf("accent" to it)).named("accent") } }
        ?: themeDefaults(dark, systemAccent = true).primary
    Surface(Modifier.size(16.dp), CircleShape, color = color) {}
}
