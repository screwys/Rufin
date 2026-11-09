package io.github.screwys.rufin.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.Animatable
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.util.lerp
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.layout.positionInParent
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion

/** Choices in one pill track; the picked one sits on a light pill that glides to it, also while the
 *  segments change width. `equal` gives every segment the same width; otherwise, given `scroll`, the
 *  segments scroll sideways when they are wider than the track. */
@Composable
internal fun <T> SegmentedPill(options: List<T>, selected: T, onSelect: (T) -> Unit, modifier: Modifier = Modifier,
    equal: Boolean = false, scroll: ScrollState? = null, enabled: Boolean = true, segment: @Composable (T, Boolean) -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val reduceMotion = LocalReduceMotion.current
    val bounds = remember { mutableStateMapOf<Int, Pair<Float, Float>>() }
    val index = options.indexOf(selected)
    // Moving to another segment, the pill blends from where it was drawn to where that segment is now,
    // so it follows a segment that grows or shrinks meanwhile instead of chasing it.
    val progress = remember { Animatable(1f) }
    var from by remember { mutableStateOf<Pair<Float, Float>?>(null) }
    val drawn = remember { floatArrayOf(Float.NaN, 0f) }
    LaunchedEffect(index) {
        if (drawn[0].isNaN()) return@LaunchedEffect
        from = drawn[0] to drawn[1]
        progress.snapTo(0f)
        progress.animateTo(1f, if (reduceMotion) tween(0) else spring(dampingRatio = .86f, stiffness = Spring.StiffnessMediumLow))
    }
    val pill = scheme.inverseSurface
    Box(modifier.alpha(if (enabled) 1f else .38f).clip(CircleShape).background(scheme.onSurface.copy(alpha = .1f)).padding(4.dp)) {
    Box(Modifier.clip(CircleShape).then(if (scroll != null && !equal) Modifier.horizontalScroll(scroll) else Modifier)) {
        Row(Modifier.then(if (equal) Modifier.fillMaxWidth() else Modifier).selectableGroup().drawBehind {
            val to = bounds[index] ?: return@drawBehind
            val (fromX, fromWidth) = from ?: to
            val x = lerp(fromX, to.first, progress.value)
            val width = lerp(fromWidth, to.second, progress.value)
            drawn[0] = x
            drawn[1] = width
            drawRoundRect(pill, Offset(x, 0f), Size(width, size.height), CornerRadius(size.height / 2))
        }, verticalAlignment = Alignment.CenterVertically) {
            options.forEachIndexed { index, option ->
                val picked = option == selected
                val content by animateColorAsState(if (picked) scheme.inverseOnSurface else scheme.onSurface,
                    tween(if (reduceMotion) 0 else 90), label = "segmentContent")
                Box(Modifier.then(if (equal) Modifier.weight(1f) else Modifier)
                    .onPlaced { bounds[index] = it.positionInParent().x to it.size.width.toFloat() }
                    .clip(CircleShape).semantics { this.selected = picked; role = Role.Tab }
                    .clickable(enabled = enabled) { onSelect(option) }.padding(horizontal = 16.dp, vertical = 10.dp),
                    contentAlignment = Alignment.Center) {
                    CompositionLocalProvider(LocalContentColor provides content) { segment(option, picked) }
                }
            }
        }
    }
    }
}
