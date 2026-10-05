package io.github.screwys.rufin.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.animation.animateBounds
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.layout.LookaheadScope
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.zIndex
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon

@Composable
internal fun SettingsPage(modifier: Modifier = Modifier, embedded: Boolean = false, content: @Composable ColumnScope.() -> Unit) {
    Column(
        if (embedded) modifier.fillMaxWidth() else modifier.fillMaxSize().verticalScroll(rememberScrollState())
            .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp + LocalPlayerBottomPadding.current),
        verticalArrangement = Arrangement.spacedBy(24.dp),
        content = content,
    )
}

@Composable
internal fun SettingsGroup(title: String? = null, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        title?.let {
            Text(it, Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary)
        }
        Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)),
            verticalArrangement = Arrangement.spacedBy(3.dp), content = content)
    }
}

@Composable
internal fun SettingsRow(
    title: String,
    modifier: Modifier = Modifier,
    summary: String? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    shape: Shape = RoundedCornerShape(4.dp),
    selected: Boolean = false,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = summary?.let { { Text(it) } },
        leadingContent = leading,
        trailingContent = trailing,
        colors = ListItemDefaults.colors(containerColor = if (selected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.clip(shape).then(modifier),
    )
}

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

@Composable
internal fun settingsChipColors() = FilterChipDefaults.filterChipColors(
    containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
    labelColor = MaterialTheme.colorScheme.onSurface,
    selectedContainerColor = MaterialTheme.colorScheme.secondaryContainer,
    selectedLabelColor = MaterialTheme.colorScheme.onSecondaryContainer,
    disabledContainerColor = MaterialTheme.colorScheme.surfaceContainer,
    disabledSelectedContainerColor = MaterialTheme.colorScheme.surfaceContainer,
)

internal fun settingsRowShape(index: Int, count: Int) = RoundedCornerShape(
    topStart = if (index == 0) 18.dp else 4.dp,
    topEnd = if (index == 0) 18.dp else 4.dp,
    bottomStart = if (index == count - 1) 18.dp else 4.dp,
    bottomEnd = if (index == count - 1) 18.dp else 4.dp,
)

@Composable
internal fun SettingsReorderButtons(canMoveUp: Boolean, canMoveDown: Boolean, moveUp: () -> Unit, moveDown: () -> Unit) {
    Row {
        IconButton(moveUp, Modifier.size(40.dp), enabled = canMoveUp) { RufinIcon("rufin-go-up-symbolic", translate("Move up"), Modifier.size(20.dp)) }
        IconButton(moveDown, Modifier.size(40.dp), enabled = canMoveDown) { RufinIcon("rufin-go-down-symbolic", translate("Move down"), Modifier.size(20.dp)) }
    }
}

internal data class SettingsReorderItem(val id: String, val title: String, val enabled: Boolean, val reorderable: Boolean = true)

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
        SettingsRow(item.title, shape = settingsRowShape(index, order.size),
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
                Row(verticalAlignment = Alignment.CenterVertically) {
                    SettingsReorderButtons(up, down, { move(id, true) }, { move(id, false) })
                    Switch(item.enabled, { onToggle(id, it) }, Modifier.semantics { contentDescription = item.title })
                }
            })
        }
    } }
    }
    }
}
