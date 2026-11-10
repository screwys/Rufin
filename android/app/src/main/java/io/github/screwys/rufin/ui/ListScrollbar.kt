package io.github.screwys.rufin.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.unit.dp

/** A thin scrollbar down a list's edge: where the list is, and a hold and drag moves it. The thumb keeps
 *  one length and, while held, follows the finger itself; the list is sent to the matching pixel, so rows
 *  still loading never move the thumb. Nothing while the list fits its pane. */
@Composable
internal fun ListScrollbar(state: LazyListState, modifier: Modifier = Modifier) {
    // The list is its first item (a page's header, any height) and then rows of one height: the middle
    // one of those in view stands for all, loaded or not.
    var head by remember(state) { mutableStateOf(0) }
    val metrics by remember(state) { derivedStateOf {
        val info = state.layoutInfo
        val shown = info.visibleItemsInfo
        val total = info.totalItemsCount
        if (total < 2 || shown.isEmpty()) return@derivedStateOf null
        shown.firstOrNull { it.index == 0 }?.let { head = it.size }
        val rows = shown.filter { it.index > 0 }.map { it.size }.sorted()
        val row = (rows.getOrNull(rows.size / 2) ?: return@derivedStateOf null).coerceAtLeast(1)
        val viewport = info.viewportEndOffset - info.viewportStartOffset
        val range = head + (total - 1) * row + info.afterContentPadding - viewport
        if (range <= row / 2) return@derivedStateOf null
        val first = shown.first()
        val scrolled = (if (first.index == 0) 0 else head + (first.index - 1) * row) - first.offset
        Triple(row, range, scrolled.toFloat() / range)
    } }
    val (row, range, at) = metrics ?: return
    var held by remember { mutableStateOf<Float?>(null) }
    BoxWithConstraints(modifier.fillMaxHeight().width(20.dp)) {
        val track = constraints.maxHeight.toFloat()
        val thumb = (track * track / (track + range)).coerceIn(with(LocalDensity.current) { 48.dp.toPx() }.coerceAtMost(track), track)
        val travel = (track - thumb).coerceAtLeast(1f)
        val fraction = (held ?: at).coerceIn(0f, 1f)
        Box(Modifier.fillMaxSize().pointerInput(state, travel, thumb, range, row, head) {
            var grab = 0f
            fun move(y: Float) {
                val to = ((y - grab) / travel).coerceIn(0f, 1f)
                held = to
                val place = (to * range).toInt()
                if (place < head) state.requestScrollToItem(0, place)
                else state.requestScrollToItem(1 + (place - head) / row, (place - head) % row)
            }
            detectVerticalDragGestures(onDragStart = { start ->
                // Grabbed on the thumb it keeps its place under the finger; elsewhere it comes to the finger.
                val top = (held ?: at).coerceIn(0f, 1f) * travel
                grab = if (start.y in top..top + thumb) start.y - top else thumb / 2
                move(start.y)
            }, onDragEnd = { held = null }, onDragCancel = { held = null }) { change, _ ->
                change.consume()
                move(change.position.y)
            }
        })
        Box(Modifier.align(Alignment.TopEnd).padding(end = 3.dp).offset { IntOffset(0, (travel * fraction).toInt()) }
            .size(if (held != null) 6.dp else 4.dp, with(LocalDensity.current) { thumb.toDp() }).clip(CircleShape)
            .background(MaterialTheme.colorScheme.onSurface.copy(alpha = if (held != null) .6f else .35f)))
    }
}
