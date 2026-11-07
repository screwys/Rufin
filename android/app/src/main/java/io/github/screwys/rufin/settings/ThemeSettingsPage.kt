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
import io.github.screwys.rufin.shared.rufin_core.AccentPreference
import io.github.screwys.rufin.shared.rufin_core.ThemePreference
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
    val themes = listOf(ThemePreference.System to translate("System"), ThemePreference.Light to translate("Light"),
        ThemePreference.Dark to translate("Dark")) +
        settings.themes.filter { it.id != "builtin:light" && it.id != "builtin:dark" }.map { ThemePreference.Named(it.id) to it.name }
    LazyVerticalGrid(GridCells.Adaptive(96.dp), Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp + LocalPlayerBottomPadding.current),
        horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(themes, key = { it.second + it.first.toString() }) { (choice, title) ->
            val themeId = when (choice) {
                is ThemePreference.System -> if (systemDark) "builtin:dark" else "builtin:light"
                is ThemePreference.Light -> "builtin:light"
                is ThemePreference.Dark -> "builtin:dark"
                is ThemePreference.Named -> choice.v1
            }
            val theme = settings.themes.find { it.id == themeId }
            val selected = settings.theme == choice
            val standard = choice !is ThemePreference.Named
            val colors = MaterialTheme.colorScheme
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Surface(Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { preferences.selectTheme(choice) }
                    .semantics { contentDescription = title },
                    shape = RoundedCornerShape(12.dp), color = colors.surfaceContainer,
                    border = BorderStroke(if (selected) 2.dp else 1.dp, if (selected) colors.primary else colors.outlineVariant)) {
                    ThemePreview(theme, theme?.dark ?: systemDark,
                        standard && settings.accent == AccentPreference.SYSTEM,
                        Modifier.padding(5.dp).fillMaxWidth().aspectRatio(9f / 16f))
                }
                Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(title, Modifier.weight(1f).padding(start = 2.dp),
                        style = MaterialTheme.typography.labelMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    if (standard) ThemeAccentMenu(
                        settings.accents.map { AccentEntry(translate(it.title), it.color, it.accent == settings.accent) { preferences.selectAccent(it.accent) } },
                        theme?.dark ?: systemDark)
                    else if ((theme?.accents?.size ?: 0) > 1) ThemeAccentMenu(
                        theme!!.accents.map { AccentEntry(it.name, it.color, it.id == theme.accentId) { preferences.selectThemeAccent(themeId, it.id) } },
                        theme.dark)
                }
            }
        }
        items(settings.errors + preferences.themeErrors, span = { GridItemSpan(maxLineSpan) }) {
            io.github.screwys.rufin.ui.ErrorNotice(it)
        }
    }
}

/** One accent color a menu offers. */
private class AccentEntry(val name: String, val color: String?, val selected: Boolean, val select: () -> Unit)

@Composable
private fun ThemeAccentMenu(options: List<AccentEntry>, dark: Boolean) {
    var expanded by remember { mutableStateOf(false) }
    val current = options.find { it.selected } ?: options.first()
    val label = translate("Accent color")
    Box {
        IconButton({ expanded = true }, Modifier.semantics {
            contentDescription = label
            stateDescription = current.name
        }) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                AccentSwatch(current.color, dark)
                RufinIcon("rufin-go-down-symbolic", null, Modifier.size(12.dp))
            }
        }
        DropdownMenu(expanded, { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option.name) },
                    leadingIcon = { AccentSwatch(option.color, dark) },
                    trailingIcon = {
                        if (option === current) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp))
                    },
                    onClick = { option.select(); expanded = false },
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
