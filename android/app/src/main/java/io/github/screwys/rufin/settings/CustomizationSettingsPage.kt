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
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.shared.rufin_core.ContextMenuItem
import io.github.screwys.rufin.shared.rufin_core.DisplaySize
import io.github.screwys.rufin.shared.rufin_core.HomeBlockRow
import io.github.screwys.rufin.shared.rufin_core.QuickMixChoice
import io.github.screwys.rufin.shared.rufin_core.QuickMixChoiceKind
import io.github.screwys.rufin.shared.rufin_core.QuickMixEditor
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInParent
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.zIndex
import io.github.screwys.rufin.ui.RufinSlider
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import kotlin.math.roundToInt
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SegmentedPill

internal enum class CustomizationTab(override val title: String) : SettingsTab { Layout("Layout"), Home("Home"), Player("Player"), Menus("Menus") }

@Composable
internal fun CustomizationSettingsPage(preferences: PreferencesConnection, browse: BrowseConnection, player: PlayerConnection) {
    var tab by rememberSaveable { mutableStateOf(CustomizationTab.Layout) }
    SearchedTab<CustomizationTab> { tab = it }
    Column(Modifier.fillMaxSize()) {
        SegmentedPill(CustomizationTab.entries, tab, { tab = it }, Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            equal = true) { option, _ ->
            Text(translate(option.title), style = MaterialTheme.typography.titleSmall, maxLines = 1)
        }
        SettingsPage(Modifier.weight(1f)) {
            if (preferences.state == null || player.settings == null)
                Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            else when (tab) {
                CustomizationTab.Layout -> LayoutTab(preferences, browse)
                CustomizationTab.Home -> HomeTab(preferences, browse)
                CustomizationTab.Player -> PlayerTab(player)
                CustomizationTab.Menus -> MenusTab(preferences)
            }
        }
    }
}

@Composable
private fun LayoutTab(preferences: PreferencesConnection, browse: BrowseConnection) {
    SettingsGroup(translate("Accessibility")) {
        SettingsSwitch(translate("Reduce motion"), preferences.reduceMotion) { preferences.changePreference { setReduceMotion(it) } }
    }
    SettingsGroup(translate("Grids")) {
        SettingsSwitch(translate("Show a badge for favorited items"), preferences.gridFavoriteBadges) { preferences.changePreference { setGridFavoriteBadges(it) } }
        SettingsSwitch(translate("Show star ratings"), preferences.gridStarRatings) { preferences.changePreference { setGridStarRatings(it) } }
    }
    SettingsGroup(translate("Library items")) {
        SettingsReorderRows(browse.routeSettings.map { SettingsReorderItem(it.descriptor.id, it.descriptor.title, it.visible) },
            onReorder = { _, id, target, after -> browse.reorderRoute(id, target, after) },
            onStep = browse::moveRoute,
            onToggle = browse::setRouteVisible)
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

/** Home's Size and Spacing, its blocks: their order, which show, and each one's item count and rows, and Quick mix. */
@Composable
private fun HomeTab(preferences: PreferencesConnection, browse: BrowseConnection) {
    val application = preferences.applicationSettings ?: return
    val home = application.home
    val rows = remember(application.homeBlocks, home) { preferences.nativeSettings().homeBlockRows() }
    val block = { id: String -> rows.first { it.block.name == id }.block }
    SettingsGroup(translate("Display")) {
        PreferenceChoice(translate("Size"), home.size, listOf(DisplaySize.COMPACT to translate("Compact"),
            DisplaySize.DEFAULT to translate("Default"), DisplaySize.LARGE to translate("Large"))) { size ->
            preferences.changePreference { setHomeSize(size) }
        }
        PreferenceChoice(translate("Spacing"), home.spacing, listOf(DisplaySize.COMPACT to translate("Small"),
            DisplaySize.DEFAULT to translate("Default"), DisplaySize.LARGE to translate("Wide"))) { spacing ->
            preferences.changePreference { setHomeSpacing(spacing) }
        }
    }
    SettingsGroup(translate("Blocks")) {
        SettingsReorderRows(rows.map { SettingsReorderItem(it.block.name, translate(it.title), it.visible, it.visible, it.canToggle, details = it.visible && it.hasOptions) },
            onReorder = { _, id, target, after -> preferences.changePreference { reorderHomeBlock(block(id), block(target), after) } },
            onToggle = { id, on -> preferences.changePreference { setHomeBlockVisible(block(id), on) } },
            details = { id -> rows.find { it.block.name == id }?.let { HomeBlockOptions(preferences, it) } })
    }
    QuickMixGroup(preferences, browse)
}

/** Quick mix's entries in their Home order, then the templates no entry was copied from, as chips, and under
 *  them the user's smart playlists, as GTK's Quick mix group. An entry's check removes it, a + adds while
 *  another fits, and a long press drags an entry to another's place. Every edit is core's; the chips then
 *  show what it saved. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun QuickMixGroup(preferences: PreferencesConnection, browse: BrowseConnection) {
    val library = browse.library ?: return
    val saved = preferences.applicationSettings?.quickMix
    var editor by remember { mutableStateOf<QuickMixEditor?>(null) }
    LaunchedEffect(library, saved) { browse.quickMixEditor()?.let { editor = it } }
    val shown = editor ?: return
    val scheme = MaterialTheme.colorScheme
    fun entry(choice: QuickMixChoice) = (choice.kind as? QuickMixChoiceKind.Entry)?.index
    val entries = shown.choices.filter { entry(it) != null }
    // The entries as a drag moves them, by their place in Quick mix; a drop commits the move.
    var order by remember(shown) { mutableStateOf(entries.map { entry(it)!! }) }
    var original by remember(shown) { mutableStateOf(order) }
    var dragged by remember(shown) { mutableStateOf<UInt?>(null) }
    var pointer by remember(shown) { mutableStateOf(Offset.Zero) }
    var grip by remember(shown) { mutableStateOf(Offset.Zero) }
    var over by remember(shown) { mutableStateOf<UInt?>(null) }
    val bounds = remember(shown) { mutableStateMapOf<UInt, Rect>() }
    fun commit(from: Int, to: Int) {
        if (from != to) preferences.changePreference { reorderQuickMix(from.toUInt(), to.toUInt(), to > from) }
    }
    fun move(index: UInt, step: Int) {
        val from = order.indexOf(index)
        val to = from + step
        if (to !in order.indices) return
        order = order.toMutableList().apply { add(to, removeAt(from)) }
        commit(from, to)
    }
    SettingsGroup(translate("Mixes")) {
        Column(Modifier.fillMaxWidth().background(scheme.surfaceContainer)) {
            FlowRow(Modifier.fillMaxWidth().padding(12.dp).pointerInput(shown) {
                detectDragGesturesAfterLongPress(
                    onDragStart = { at ->
                        val hit = bounds.entries.firstOrNull { it.value.contains(at) } ?: return@detectDragGesturesAfterLongPress
                        dragged = hit.key; pointer = at; grip = at - hit.value.topLeft; original = order; over = null
                    },
                    onDrag = { change, amount ->
                        val moving = dragged ?: return@detectDragGesturesAfterLongPress
                        change.consume()
                        pointer += amount
                        // Entering another entry takes its place; the one passed steps aside.
                        val under = bounds.entries.firstOrNull { it.key != moving && it.value.contains(pointer) }?.key
                        if (under != null && under != over) {
                            val to = order.indexOf(under)
                            order = order.toMutableList().apply { remove(moving); add(to, moving) }
                        }
                        over = under
                    },
                    onDragEnd = {
                        dragged?.let { moving -> commit(original.indexOf(moving), order.indexOf(moving)) }
                        dragged = null
                    },
                    onDragCancel = { order = original; dragged = null },
                )
            }, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                order.forEachIndexed { place, index -> key(index) {
                    val choice = entries.first { entry(it) == index }
                    val lifted = dragged == index
                    QuickMixChip(choice, shown.canAdd, browse, preferences, Modifier
                        .onPlaced { bounds[index] = it.boundsInParent() }
                        .zIndex(if (lifted) 1f else 0f)
                        .graphicsLayer {
                            if (lifted) bounds[index]?.let { at ->
                                translationX = pointer.x - grip.x - at.left
                                translationY = pointer.y - grip.y - at.top
                            }
                        }
                        .then(if (lifted) Modifier.shadow(6.dp, CircleShape) else Modifier)
                        // Screen readers move an entry a place at a time.
                        .semantics {
                            customActions = listOfNotNull(
                                CustomAccessibilityAction(translate("Move up")) { move(index, -1); true }.takeIf { place > 0 },
                                CustomAccessibilityAction(translate("Move down")) { move(index, 1); true }.takeIf { place < order.lastIndex })
                        })
                } }
                shown.choices.filter { entry(it) == null }.forEach { choice -> QuickMixChip(choice, shown.canAdd, browse, preferences) }
            }
            // Under a line across the container, the user's smart playlists.
            if (shown.playlists.isNotEmpty()) {
                HorizontalDivider(color = scheme.onSurface.copy(alpha = .08f))
                Text(translate("Smart playlists"), Modifier.padding(start = 12.dp, end = 12.dp, top = 12.dp, bottom = 8.dp),
                    style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold, color = scheme.onSurfaceVariant)
                FlowRow(Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    shown.playlists.forEach { choice -> QuickMixChip(choice, shown.canAdd, browse, preferences) }
                }
            }
        }
    }
}

/** A Quick mix choice: its glyph and name, then a filled check that removes an entry, or a + that adds a
 *  template or a smart playlist while another entry fits. What is not in Quick mix is faded. */
@Composable
private fun QuickMixChip(choice: QuickMixChoice, canAdd: Boolean, browse: BrowseConnection, preferences: PreferencesConnection,
    modifier: Modifier = Modifier) {
    val scheme = MaterialTheme.colorScheme
    val faded = if (choice.added) 1f else .5f
    Row(modifier.height(40.dp).clip(CircleShape).background(if (choice.added) scheme.surfaceContainerHighest else scheme.surfaceContainerHigh)
        .padding(start = 12.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        RufinIcon(choice.icon, null, Modifier.size(16.dp).alpha(faded))
        Text(choice.title, Modifier.weight(1f, fill = false).alpha(faded), style = MaterialTheme.typography.bodyLarge,
            fontWeight = if (choice.added) FontWeight.SemiBold else FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
        val kind = choice.kind
        if (choice.added) {
            val index = (kind as? QuickMixChoiceKind.Entry)?.index
            FilledIconButton({ index?.let { preferences.changePreference { removeQuickMix(it) } } }, Modifier.size(32.dp),
                colors = IconButtonDefaults.filledIconButtonColors(containerColor = scheme.secondaryContainer,
                    contentColor = scheme.onSecondaryContainer)) {
                RufinIcon("rufin-object-select-symbolic", translate("Remove"), Modifier.size(14.dp))
            }
        } else IconButton({
            when (kind) {
                is QuickMixChoiceKind.Template -> preferences.changePreference { addQuickMixTemplate(kind.name) }
                is QuickMixChoiceKind.Playlist -> browse.addQuickMixPlaylist(kind.objectId)
                is QuickMixChoiceKind.Entry -> {}
            }
        }, Modifier.size(32.dp), enabled = canAdd) {
            RufinIcon("rufin-list-add-symbolic", translate("Add"), Modifier.size(14.dp))
        }
    }
}

/** A shown Home block's own choices, under its row: how many items it lists and the rows its columns stack. */
@Composable
private fun HomeBlockOptions(preferences: PreferencesConnection, row: HomeBlockRow) {
    Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        val muted = MaterialTheme.colorScheme.onSurfaceVariant
        var items by remember(row.items) { mutableFloatStateOf(row.items.toFloat()) }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate("Items"), Modifier.width(48.dp), style = MaterialTheme.typography.bodyMedium, color = muted)
            RufinSlider(items, { items = it.roundToInt().toFloat() }, Modifier.weight(1f).semantics { contentDescription = translate("Items") },
                valueRange = 1f..24f, steps = 22,
                onValueChangeFinished = { preferences.changePreference { setHomeBlockItems(row.block, items.roundToInt().toUByte()) } })
            Text(items.roundToInt().toString(), Modifier.width(24.dp), style = MaterialTheme.typography.labelLarge.copy(fontFeatureSettings = "tnum"),
                textAlign = TextAlign.End)
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate("Rows"), Modifier.width(48.dp), style = MaterialTheme.typography.bodyMedium, color = muted)
            SegmentedPill(listOf(0, 1, 2, 3), row.rows?.toInt() ?: 0, { rows ->
                preferences.changePreference { setHomeBlockRows(row.block, rows.takeIf { it > 0 }?.toUByte()) }
            }) { rows, _ ->
                Text(if (rows == 0) translate("Automatic") else rows.toString(), style = MaterialTheme.typography.titleSmall, maxLines = 1)
            }
        }
    }
}

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
internal val menuPlays = listOf(
    "Play" to "rufin-media-playback-start-symbolic",
    "Play Next" to "rufin-mail-forward-symbolic",
    "Play Later" to "rufin-go-last-symbolic")
internal val menuActions = listOf(
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
                            RufinIcon(if (index < 3) "rufin-starred-symbolic" else "rufin-star-outline-symbolic", null, Modifier.size(24.dp),
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
        Surface(Modifier.fillMaxWidth().clip(RoundedCornerShape(5.dp)).then(searchHighlight(title)),
            color = MaterialTheme.colorScheme.surfaceContainer, shape = RoundedCornerShape(5.dp)) {
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
