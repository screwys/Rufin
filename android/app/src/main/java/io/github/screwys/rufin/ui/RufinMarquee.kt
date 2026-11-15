package io.github.screwys.rufin.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion
import kotlinx.coroutines.delay

/** Text wider than its room slides once, as GTK's does: after a pause it glides until its cut end is in
 *  view, rests there, and goes back to its start. It never runs round. */
@Composable
internal fun Modifier.rufinMarquee(): Modifier {
    var overflow by remember { mutableIntStateOf(0) }
    val offset = remember { Animatable(0f) }
    val still = LocalReduceMotion.current
    val speed = with(LocalDensity.current) { 28.dp.toPx() }
    LaunchedEffect(overflow, still) {
        offset.snapTo(0f)
        if (still || overflow <= 0) return@LaunchedEffect
        delay(2_000)
        offset.animateTo(overflow.toFloat(), tween((overflow / speed * 1_000).toInt().coerceAtLeast(200), easing = LinearEasing))
        delay(2_000)
        offset.animateTo(0f, tween(300))
    }
    return clipToBounds().layout { measurable, constraints ->
        val placeable = measurable.measure(Constraints(minHeight = constraints.minHeight, maxHeight = constraints.maxHeight))
        val width = placeable.width.coerceIn(constraints.minWidth, constraints.maxWidth)
        overflow = placeable.width - width
        layout(width, placeable.height) { placeable.placeRelative(-offset.value.toInt(), 0) }
    }
}
