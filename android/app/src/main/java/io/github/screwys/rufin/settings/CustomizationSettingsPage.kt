package io.github.screwys.rufin.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.browse.BrowseConnection
import io.github.screwys.rufin.browse.BrowseSwipeAction
import io.github.screwys.rufin.browse.MediaMenuDivider
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.more.MoreDestination
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.shared.rufin_core.ContextMenuItem
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SegmentedPill

private enum class CustomizationTab(val title: String) { Layout("Layout"), Player("Player"), Menus("Menus") }

@Composable
internal fun CustomizationSettingsPage(preferences: PreferencesConnection, browse: BrowseConnection,
    player: PlayerConnection, navigate: (MoreDestination) -> Unit) {
    var tab by rememberSaveable { mutableStateOf(CustomizationTab.Layout) }
    Column(Modifier.fillMaxSize()) {
        SegmentedPill(CustomizationTab.entries, tab, { tab = it }, Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            equal = true) { option, _ ->
            Text(translate(option.title), style = MaterialTheme.typography.titleSmall, maxLines = 1)
        }
        SettingsPage(Modifier.weight(1f)) {
            if (preferences.state == null || player.settings == null)
                Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            else when (tab) {
                CustomizationTab.Layout -> LayoutTab(preferences, browse, navigate)
                CustomizationTab.Player -> PlayerTab(player)
                CustomizationTab.Menus -> MenusTab(preferences)
            }
        }
    }
}

@Composable
private fun LayoutTab(preferences: PreferencesConnection, browse: BrowseConnection, navigate: (MoreDestination) -> Unit) {
    SettingsGroup(translate("Accessibility")) {
        SettingsSwitch(translate("Reduce motion"), preferences.reduceMotion) { preferences.changePreference { setReduceMotion(it) } }
    }
    SettingsGroup(translate("Navigation")) {
        val routes = browse.routeSettings
        SettingsLink(null, translate("Library items"), shownCount(routes.count { it.visible }, routes.size)) { navigate(MoreDestination.Routes) }
        val blocks = preferences.applicationSettings?.homeBlocks
        val rows = remember(blocks) { if (blocks == null) emptyList() else preferences.nativeSettings().homeBlockRows() }
        SettingsLink(null, translate("Home blocks"), shownCount(rows.count { it.visible }, rows.size)) { navigate(MoreDestination.HomeSettings) }
    }
    SettingsGroup(translate("Swipe on a row")) {
        val swipeActions = BrowseSwipeAction.entries.map { it.name to it.label }
        val preview: @Composable (String) -> Unit = { id -> BrowseSwipeAction.entries.find { it.name == id }?.let { RufinIcon(it.icon, null, Modifier.size(24.dp)) } }
        PreferenceChoice(translate("Swipe right"), preferences.swipeRight.name, swipeActions, preview = preview,
            select = { preferences.selectSwipeAction(false, it) })
        PreferenceChoice(translate("Swipe left"), preferences.swipeLeft.name, swipeActions, preview = preview,
            select = { preferences.selectSwipeAction(true, it) })
    }
}

private fun shownCount(shown: Int, total: Int) =
    translate("{shown} of {total} shown").replace("{shown}", shown.toString()).replace("{total}", total.toString())

/** The fullscreen player's switches and the seekbar, as GTK's Player tab has them. */
@Composable
private fun PlayerTab(player: PlayerConnection) {
    val state = player.settings ?: return
    val display = state.display
    fun change(edit: SettingsOwner.() -> Unit) { player.editPreferences(edit) }
    SettingsGroup(translate("Seekbar")) {
        SettingsSwitch(translate("Waveform seekbar"), display.seekbarWaveformEnabled) { change { setSeekbarWaveformEnabled(it) } }
    }
    SettingsGroup(translate("Fullscreen player")) {
        SettingsSwitch(translate("Show lyrics"), display.fullscreenLyricsVisible) { change { setFullscreenLyricsVisible(it) } }
        SettingsSwitch(translate("Show current lyrics line"), display.fullscreenCurrentLyricsLineVisible) { change { setFullscreenCurrentLyricsLineVisible(it) } }
        SettingsSwitch(translate("Show visualizer"), display.fullscreenVisualizerVisible) { change { setFullscreenVisualizerVisible(it) } }
        SettingsSwitch(translate("Combine lyrics and visualizer"), state.rightPanel.combined) { change { setRightPanelCombined(it) } }
        SettingsSwitch(translate("Dynamic background"), display.fullscreenDynamicBackground) { change { setFullscreenDynamicBackground(it) } }
        SettingsSwitch(translate("Enable background image"), display.fullscreenBackgroundImage, enabled = display.fullscreenDynamicBackground) {
            change { setFullscreenBackgroundImage(it) }
        }
    }
}

/** The long-press menu's entries in its order, with their icons; the Play setting is its row of big buttons. */
private val menuPlays = listOf(
    "Play" to "rufin-media-playback-start-symbolic",
    "Play Next" to "rufin-mail-forward-symbolic",
    "Play Later" to "rufin-go-last-symbolic")
private val menuActions = listOf(
    Triple(ContextMenuItem.PLAY_RADIO, "Play radio", "rufin-audio-only-symbolic"),
    Triple(ContextMenuItem.ADD_TO_PLAYLIST, "Add to Playlist", "rufin-playlists-symbolic"),
    Triple(ContextMenuItem.FAVORITES, "Favorites", "rufin-heart-outline-symbolic"),
    Triple(ContextMenuItem.DOWNLOAD, "Download", "rufin-download-symbolic"),
    Triple(ContextMenuItem.GO_TO, "Go to", "rufin-external-link-compact-symbolic"),
    Triple(ContextMenuItem.SHARE, "Share", "rufin-share-symbolic"),
    Triple(ContextMenuItem.EDIT_METADATA, "Edit metadata", "rufin-document-edit-symbolic"),
    Triple(ContextMenuItem.PINS, "Saved", "rufin-bookmark-symbolic"))

/** The menu a long press opens, as albums, artists and tracks show it; each entry shows or hides itself. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MenusTab(preferences: PreferencesConnection) {
    val menu = preferences.applicationSettings?.contextMenu ?: return
    fun visible(item: ContextMenuItem) = menu.items.find { it.item == item }?.visible ?: true
    fun show(item: ContextMenuItem, shown: Boolean) = preferences.changePreference {
        val current = applicationPreferences().contextMenu
        setContextMenu(current.copy(items = current.items.map { if (it.item == item) it.copy(visible = shown) else it }))
    }
    SettingsGroup(translate("Item menus"), description = translate("The sheet a long press opens. Tap an entry to show or hide it.")) {
        Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(18.dp), color = MaterialTheme.colorScheme.surfaceContainer) {
            Column(Modifier.fillMaxWidth().padding(vertical = 10.dp)) {
                BottomSheetDefaults.DragHandle(Modifier.align(Alignment.CenterHorizontally))
                // The header, as the item's cover and names.
                Row(Modifier.padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Surface(Modifier.size(44.dp), RoundedCornerShape(8.dp), MaterialTheme.colorScheme.onSurface.copy(alpha = .12f)) {}
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Surface(Modifier.size(130.dp, 8.dp), CircleShape, MaterialTheme.colorScheme.onSurface.copy(alpha = .5f)) {}
                        Surface(Modifier.size(90.dp, 6.dp), CircleShape, MaterialTheme.colorScheme.onSurface.copy(alpha = .25f)) {}
                    }
                }
                MediaMenuDivider()
                val playsShown = visible(ContextMenuItem.PLAY)
                MenuEntryRow(playsShown, { show(ContextMenuItem.PLAY, it) }) {
                    Row(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        menuPlays.forEach { (label, icon) ->
                            Column(Modifier.weight(1f).padding(vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally,
                                verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                RufinIcon(icon, null, Modifier.size(28.dp).alpha(if (playsShown) 1f else .38f), glyph = true)
                                MenuEntryLabel(label, playsShown, MaterialTheme.typography.labelMedium)
                            }
                        }
                    }
                }
                menuActions.forEach { (item, label, icon) ->
                    val shown = visible(item)
                    MenuEntryRow(shown, { show(item, it) }) {
                        RufinIcon(icon, null, Modifier.size(24.dp).alpha(if (shown) 1f else .38f))
                        MenuEntryLabel(label, shown, MaterialTheme.typography.bodyLarge, Modifier.weight(1f))
                    }
                }
                MediaMenuDivider()
                MenuEntryRow(menu.ratingVisible, { shown ->
                    preferences.changePreference { setContextMenu(applicationPreferences().contextMenu.copy(ratingVisible = shown)) }
                }) {
                    Row(Modifier.weight(1f).alpha(if (menu.ratingVisible) 1f else .38f)) {
                        repeat(5) { index ->
                            RufinIcon(if (index < 3) "rufin-starred-symbolic" else "rufin-non-starred-symbolic", null, Modifier.size(24.dp),
                                if (index < 3) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    MenuEntryLabel("Rating", menu.ratingVisible, MaterialTheme.typography.labelMedium)
                }
            }
        }
    }
}

/** A shown entry has its check mark; a hidden one is struck through and dimmed. */
@Composable
private fun MenuEntryRow(shown: Boolean, show: (Boolean) -> Unit, content: @Composable RowScope.() -> Unit) {
    Row(Modifier.fillMaxWidth().toggleable(shown, role = Role.Checkbox, onValueChange = show).heightIn(min = 52.dp)
        .padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
        Box(Modifier.size(20.dp)) { if (shown) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(20.dp), MaterialTheme.colorScheme.primary) }
        content()
    }
}

@Composable
private fun MenuEntryLabel(label: String, shown: Boolean, style: androidx.compose.ui.text.TextStyle, modifier: Modifier = Modifier) {
    Text(translate(label), modifier, style = style, maxLines = 1,
        color = if (shown) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = .6f),
        textDecoration = if (shown) null else TextDecoration.LineThrough)
}

/** A setting that takes one of `options`. A few short ones are a pill select in the row; more, or longer
 *  ones, open a sheet of rows with the picked one highlighted, as GTK marks it. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun <T> PreferenceChoice(title: String, selected: T, options: List<Pair<T, String>>,
    preview: (@Composable (T) -> Unit)? = null, rowPreview: Boolean = true, enabled: (T) -> Boolean = { true }, select: (T) -> Unit,
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
        leading = preview?.takeIf { rowPreview }?.let { { it(selected) } },
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
