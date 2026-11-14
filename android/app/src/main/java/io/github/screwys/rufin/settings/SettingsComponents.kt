package io.github.screwys.rufin.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.animateBounds
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.foundation.clickable
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.semantics.Role
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.key
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.layout.LookaheadScope
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.zIndex
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop

@Composable
internal fun SettingsPage(modifier: Modifier = Modifier, embedded: Boolean = false, content: @Composable ColumnScope.() -> Unit) {
    if (embedded) {
        Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(24.dp), content = content)
        return
    }
    Column(modifier.fillMaxSize().verticalScroll(rememberScrollState())
        .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp + LocalPlayerBottomPadding.current),
        verticalArrangement = Arrangement.spacedBy(24.dp), content = content)
}

@Composable
internal fun SettingsGroup(title: String? = null, description: String? = null, content: @Composable ColumnScope.() -> Unit) {
    // As Navic's segmented lists: a heading in the accent over the options, each its own tonal
    // segment with a small gap, the group's outer corners round.
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        title?.let {
            Text(it, Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary)
        }
        description?.let {
            Text(it, Modifier.padding(start = 12.dp, end = 12.dp, bottom = 2.dp), style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)),
            verticalArrangement = Arrangement.spacedBy(3.dp), content = content)
    }
}

@Composable
internal fun SettingsExpander(title: String, content: @Composable () -> Unit) {
    var expanded by rememberSaveable { mutableStateOf(false) }
    val visibility = remember { MutableTransitionState(expanded) }
    val bringIntoView = remember { BringIntoViewRequester() }
    visibility.targetState = expanded
    LaunchedEffect(visibility) {
        snapshotFlow { visibility.isIdle && visibility.currentState }
            // Restoring an open section keeps the saved scroll position.
            .drop(1)
            .collectLatest { revealed -> if (revealed) bringIntoView.bringIntoView() }
    }
    val duration = if (LocalReduceMotion.current) 0 else 180
    val rotation by animateFloatAsState(if (expanded) 90f else 0f, tween(duration), label = "settingsExpansion")
    SettingsGroup {
        SettingsRow(title, modifier = Modifier.clickable(role = Role.Button) { expanded = !expanded },
            trailing = { RufinIcon("rufin-go-next-symbolic", translate(if (expanded) "Hide" else "Show"),
                Modifier.size(20.dp).graphicsLayer { rotationZ = rotation }) })
        AnimatedVisibility(visibleState = visibility, modifier = Modifier.bringIntoViewRequester(bringIntoView),
            enter = expandVertically(tween(duration)) + fadeIn(tween(duration)),
            exit = shrinkVertically(tween(duration)) + fadeOut(tween(duration))) { content() }
    }
}

@Composable
internal fun SettingsRow(
    title: String,
    modifier: Modifier = Modifier,
    summary: String? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    shape: Shape? = null,
    selected: Boolean = false,
    /** An action's title takes the accent, or the error color for one that removes. */
    titleColor: Color = Color.Unspecified,
) {
    ListItem(
        headlineContent = { Text(title, color = titleColor) },
        supportingContent = summary?.let { { Text(it) } },
        leadingContent = leading,
        trailingContent = trailing,
        colors = ListItemDefaults.colors(containerColor = if (selected) MaterialTheme.colorScheme.secondaryContainer
            else MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.clip(shape ?: RoundedCornerShape(5.dp)).then(modifier),
    )
}

/** A text setting: the row shows `summary`, and opens a dialog that edits `value`. */
@Composable
internal fun SettingsText(title: String, value: String, summary: String, secret: Boolean = false, enabled: Boolean = true,
    /** Whether the dialog is open, for a page that opens it from elsewhere too. */
    editing: MutableState<Boolean> = remember { mutableStateOf(false) }, save: (String) -> Unit) {
    var open by editing
    var draft by remember(value) { mutableStateOf(value) }
    SettingsRow(title, summary = summary, modifier = Modifier.clickable(enabled = enabled) { draft = value; open = true })
    if (open) AlertDialog(onDismissRequest = { open = false }, title = { Text(title) },
        text = { OutlinedTextField(draft, { draft = it }, modifier = Modifier.fillMaxWidth(), singleLine = true, colors = settingsTextFieldColors(),
            visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None) },
        confirmButton = { TextButton({ save(draft); open = false }) { Text(translate("Save")) } },
        dismissButton = { TextButton({ open = false }) { Text(translate("Cancel")) } })
}

/** What a secret's row shows: dots once one is set. */
internal fun secretSummary(value: String, empty: String) = if (value.isBlank()) empty else "••••••••••••"

@Composable
internal fun settingsTextFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedContainerColor = MaterialTheme.colorScheme.surfaceContainer,
    unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainer,
    disabledContainerColor = MaterialTheme.colorScheme.surfaceContainer,
    errorContainerColor = MaterialTheme.colorScheme.surfaceContainer,
    unfocusedBorderColor = MaterialTheme.colorScheme.outline,
    focusedBorderColor = MaterialTheme.colorScheme.primary,
)

@Composable
internal fun settingsButtonColors() = ButtonDefaults.outlinedButtonColors(
    containerColor = MaterialTheme.colorScheme.surfaceContainer,
    contentColor = MaterialTheme.colorScheme.onSurface,
    disabledContainerColor = MaterialTheme.colorScheme.surfaceContainer,
)

/** A row of a reorderable list; `toggleable` is false where its switch cannot change, such as
 * the last visible Home block. */
internal data class SettingsReorderItem(val id: String, val title: String, val enabled: Boolean, val reorderable: Boolean = true,
    val toggleable: Boolean = true, val summary: String? = null)

@Composable
internal fun SettingsReorderRows(items: List<SettingsReorderItem>,
    onReorder: (List<String>, String, String, Boolean) -> Unit,
    onToggle: (String, Boolean) -> Unit,
    onStep: ((String, Boolean) -> Unit)? = null,
) {
    var order by remember(items) { mutableStateOf(items.map { it.id }) }
    var dragged by remember { mutableStateOf<String?>(null) }
    var distance by remember { mutableFloatStateOf(0f) }
    var original by remember { mutableStateOf<List<String>>(emptyList()) }
    val heights = remember { mutableMapOf<String, Int>() }
    val gap = with(LocalDensity.current) { 3.dp.toPx() }
    val reduceMotion = LocalReduceMotion.current
    fun move(id: String, up: Boolean) {
        val from = order.indexOf(id)
        val to = from + if (up) -1 else 1
        if (to !in order.indices || !items.first { it.id == order[to] }.reorderable) return
        val target = order[to]
        order = order.toMutableList().apply { add(to, removeAt(from)) }
        if (onStep != null) onStep(id, up) else onReorder(order, id, target, !up)
    }
    LookaheadScope {
    val lookahead = this
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(3.dp)) {
    order.forEachIndexed { index, id -> key(id) {
        val item = items.first { it.id == id }
        val up = item.reorderable && index > 0 && items.first { it.id == order[index - 1] }.reorderable
        val down = item.reorderable && index < order.lastIndex && items.first { it.id == order[index + 1] }.reorderable
        val dragOffset by animateFloatAsState(if (dragged == id) distance else 0f,
            animationSpec = tween(if (reduceMotion || dragged == id) 0 else 160), label = "settingDragOffset")
        Box(Modifier.animateBounds(lookahead, boundsTransform = { _, _ -> tween(if (reduceMotion || dragged == id) 0 else 160) })
            .onSizeChanged { heights[id] = it.height }.zIndex(if (dragged == id) 1f else 0f)
            .graphicsLayer { translationY = dragOffset }) {
        SettingsRow(item.title, summary = item.summary,
            leading = {
                Box(Modifier.size(40.dp).pointerInput(id, item.reorderable, items, gap) {
                    if (item.reorderable) detectVerticalDragGestures(
                        onDragStart = { dragged = id; distance = 0f; original = order },
                        onVerticalDrag = { change, amount ->
                            change.consume()
                            distance += amount
                            val downwards = amount > 0f
                            while (true) {
                                val from = order.indexOf(id)
                                val to = from + if (downwards) 1 else -1
                                if (to !in order.indices || !items.first { it.id == order[to] }.reorderable) break
                                val threshold = (heights.getValue(id) + heights.getValue(order[to])) / 2f + gap
                                if ((if (downwards) distance else -distance) <= threshold) break
                                val displacement = heights.getValue(order[to]) + gap
                                order = order.toMutableList().apply { add(to, removeAt(from)) }
                                distance -= if (downwards) displacement else -displacement
                            }
                        },
                        onDragEnd = {
                            val from = original.indexOf(id)
                            val to = order.indexOf(id)
                            if (from != to) onReorder(order, id, original[to], to > from)
                            dragged = null; distance = 0f
                        },
                        onDragCancel = { order = original; dragged = null; distance = 0f },
                    )
                }, contentAlignment = Alignment.Center) {
                    RufinIcon("rufin-list-drag-handle-symbolic", null, Modifier.size(20.dp),
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = if (item.reorderable) 1f else .38f))
                }
            }, trailing = {
                Switch(item.enabled, { onToggle(id, it) }, Modifier.semantics { contentDescription = item.title }, enabled = item.toggleable)
            // The handle drags; screen readers move a row a place at a time.
            }, modifier = Modifier.semantics {
                customActions = listOfNotNull(
                    CustomAccessibilityAction(translate("Move up")) { move(id, true); true }.takeIf { up },
                    CustomAccessibilityAction(translate("Move down")) { move(id, false); true }.takeIf { down })
            })
        }
    } }
    }
    }
}
