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
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion

/** Rufin's slider, drawn as the player's seek line: a round thumb that grows while held, on a rounded
 *  track that is thicker and in the accent where it is filled. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RufinSlider(value: Float, onValueChange: (Float) -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true,
    valueRange: ClosedFloatingPointRange<Float> = 0f..1f, steps: Int = 0, onValueChangeFinished: (() -> Unit)? = null) {
    val interaction = remember { MutableInteractionSource() }
    val dragged by interaction.collectIsDraggedAsState()
    val pressed by interaction.collectIsPressedAsState()
    val radius by animateDpAsState(if (dragged || pressed) 10.dp else 8.dp, tween(if (LocalReduceMotion.current) 0 else 120),
        label = "sliderThumb")
    val scheme = MaterialTheme.colorScheme
    val active = if (enabled) scheme.primary else scheme.onSurface.copy(alpha = .38f)
    val inactive = scheme.onSurface.copy(alpha = if (enabled) .2f else .12f)
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    Slider(value, onValueChange, modifier, enabled, onValueChangeFinished = onValueChangeFinished, interactionSource = interaction,
        steps = steps, valueRange = valueRange,
        thumb = { Canvas(Modifier.size(20.dp)) { drawCircle(active, radius.toPx()) } },
        track = { state ->
            Canvas(Modifier.fillMaxWidth().height(20.dp)) {
                val span = state.valueRange.endInclusive - state.valueRange.start
                val fraction = if (span <= 0f) 0f else ((state.value - state.valueRange.start) / span).coerceIn(0f, 1f)
                val start = Offset(if (rtl) size.width else 0f, center.y)
                val end = Offset(if (rtl) 0f else size.width, center.y)
                drawLine(inactive, start, end, 4.dp.toPx(), StrokeCap.Round)
                if (fraction > 0f) drawLine(active, start, Offset(size.width * if (rtl) 1f - fraction else fraction, center.y),
                    6.dp.toPx(), StrokeCap.Round)
            }
        })
}
