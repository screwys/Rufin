package io.github.screwys.rufin.ui

import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import kotlin.math.abs
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion

/** Rufin's slider, drawn as the player's seek line: a round thumb that grows while held, on a rounded
 *  track that is thicker and in the accent where it is filled. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RufinSlider(value: Float, onValueChange: (Float) -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true,
    valueRange: ClosedFloatingPointRange<Float> = 0f..1f, steps: Int = 0, onValueChangeFinished: (() -> Unit)? = null,
    /** Values the thumb sticks to, drawn as stops on the track. */
    marks: List<Float> = emptyList(),
    /** Where the fill starts when that is not the range's start: zero, for a value that goes both ways. Marked by a ring. */
    origin: Float? = null) {
    val interaction = remember { MutableInteractionSource() }
    val dragged by interaction.collectIsDraggedAsState()
    val pressed by interaction.collectIsPressedAsState()
    val radius by animateDpAsState(if (dragged || pressed) 10.dp else 8.dp, tween(if (LocalReduceMotion.current) 0 else 120),
        label = "sliderThumb")
    val scheme = MaterialTheme.colorScheme
    val active = if (enabled) scheme.primary else scheme.onSurface.copy(alpha = .38f)
    val inactive = scheme.onSurface.copy(alpha = if (enabled) .2f else .12f)
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    val reach = (valueRange.endInclusive - valueRange.start) * .025f
    val ring = scheme.onSurface
    val stop = scheme.onSurface.copy(alpha = .7f)
    val filledStop = scheme.onPrimary
    val ground = scheme.surfaceContainer
    Slider(value, if (marks.isEmpty()) onValueChange else { raw -> onValueChange(marks.firstOrNull { abs(it - raw) <= reach } ?: raw) }, modifier, enabled, onValueChangeFinished = onValueChangeFinished, interactionSource = interaction,
        steps = steps, valueRange = valueRange,
        thumb = { Canvas(Modifier.size(20.dp)) { drawCircle(active, radius.toPx()) } },
        track = { state ->
            Canvas(Modifier.fillMaxWidth().height(20.dp)) {
                val span = state.valueRange.endInclusive - state.valueRange.start
                fun at(value: Float) = if (span <= 0f) 0f else ((value - state.valueRange.start) / span).coerceIn(0f, 1f)
                // The thumb's centre travels between the insets, as the marks under the track do.
                val inset = 10.dp.toPx()
                fun x(fraction: Float) = (inset + (size.width - 2 * inset) * fraction).let { if (rtl) size.width - it else it }
                val fraction = at(state.value)
                val from = origin?.let(::at)
                drawLine(inactive, Offset(if (rtl) size.width else 0f, center.y), Offset(if (rtl) 0f else size.width, center.y), 4.dp.toPx(), StrokeCap.Round)
                if (from == null) {
                    if (fraction > 0f) drawLine(active, Offset(if (rtl) size.width else 0f, center.y), Offset(x(fraction), center.y), 6.dp.toPx(), StrokeCap.Round)
                } else if (fraction != from) drawLine(active, Offset(x(from), center.y), Offset(x(fraction), center.y), 6.dp.toPx(), StrokeCap.Round)
                val filled = minOf(from ?: 0f, fraction)..maxOf(from ?: 0f, fraction)
                marks.forEach { mark ->
                    val place = Offset(x(at(mark)), center.y)
                    if (mark == origin) {
                        drawCircle(ground, 5.dp.toPx(), place)
                        drawCircle(ring, 4.dp.toPx(), place, style = Stroke(2.dp.toPx()))
                    } else drawCircle(if (at(mark) in filled) filledStop else stop, 2.dp.toPx(), place)
                }
            }
        })
}
