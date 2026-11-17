package io.github.screwys.rufin.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties
import io.github.screwys.rufin.settings.LocalReduceMotion
import kotlin.math.cos
import kotlin.math.sin

/** A favorite toggle's clicks, which its heart animates: the toggle calls [clicked] with the state it had. */
@Stable
internal class FavoriteMotion {
    internal var target by mutableStateOf(false)
        private set
    internal var clicks by mutableIntStateOf(0)
        private set

    fun clicked(favorite: Boolean) {
        target = !favorite
        clicks++
    }
}

@Composable
internal fun rememberFavoriteMotion(): FavoriteMotion = remember { FavoriteMotion() }

private val PopEasing = CubicBezierEasing(.2f, .8f, .3f, 1.4f)
private val BurstEasing = CubicBezierEasing(.1f, .7f, .3f, 1f)
private const val POP = 440
private const val DROP = 260
private const val BURST = 640

/** Every favorite toggle's heart, as GTK's and the web's: a click that makes a favorite pops the heart and
 *  sends seven small hearts out of it over its neighbours; a click that removes one shrinks it back. Reduce
 *  motion shows the state alone. While it moves, it shows the state the click asked for. */
@Composable
internal fun FavoriteHeart(favorite: Boolean, motion: FavoriteMotion, contentDescription: String?, modifier: Modifier,
    activeTint: Color = MaterialTheme.colorScheme.primary, glyph: Boolean = false) {
    val reduceMotion = LocalReduceMotion.current
    val clock = remember { Animatable(1f) }
    var moving by remember { mutableStateOf(false) }
    var popping by remember { mutableStateOf(false) }
    LaunchedEffect(motion.clicks) {
        if (motion.clicks == 0 || reduceMotion) return@LaunchedEffect
        popping = motion.target
        moving = true
        clock.snapTo(0f)
        clock.animateTo(1f, tween(if (popping) BURST else DROP, easing = LinearEasing))
        moving = false
    }
    val shown = if (moving) motion.target else favorite
    val tint = if (shown) activeTint else LocalContentColor.current
    RufinIcon(if (shown) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic", contentDescription,
        modifier.graphicsLayer {
            val scale = if (!moving) 1f else if (popping) {
                val time = (clock.value * BURST / POP).coerceAtMost(1f)
                if (time < .55f) .5f + .85f * PopEasing.transform(time / .55f)
                else 1.35f - .35f * PopEasing.transform((time - .55f) / .45f)
            } else .75f + .25f * clock.value
            scaleX = scale
            scaleY = scale
        }, tint, glyph)
    if (moving && popping) HeartBurst(clock.value)
}

/** The seven small hearts, in a popup so neither the button nor its row clips them. */
@Composable
private fun HeartBurst(time: Float) {
    // A favorite takes the accent, as on GTK and the web; every other heart a lighter tint of it.
    val accent = MaterialTheme.colorScheme.primary
    val tint = androidx.compose.ui.graphics.lerp(accent, Color.White, .45f)
    val box = 64.dp
    val half = with(LocalDensity.current) { (box / 2).roundToPx() }
    val centred = remember(half) { object : PopupPositionProvider {
        override fun calculatePosition(anchorBounds: IntRect, windowSize: IntSize, layoutDirection: LayoutDirection, popupContentSize: IntSize) =
            anchorBounds.center - IntOffset(half, half)
    } }
    Popup(centred, properties = PopupProperties(clippingEnabled = false)) {
        Box(Modifier.size(box), contentAlignment = Alignment.Center) {
            val travel = BurstEasing.transform(time)
            val fade = if (time < .7f) 1f else 1f - BurstEasing.transform((time - .7f) / .3f)
            repeat(7) { index ->
                val angle = 90f + index * 360f / 7
                val distance: Dp = 4.dp + (listOf(14.dp, 18.dp, 22.dp)[index % 3] - 4.dp) * travel
                val radians = Math.toRadians(angle.toDouble())
                RufinIcon("rufin-heart-filled-symbolic", null, Modifier.size(8.dp).graphicsLayer {
                    translationX = distance.toPx() * sin(radians).toFloat()
                    translationY = -distance.toPx() * cos(radians).toFloat()
                    rotationZ = angle
                    val scale = .4f + .6f * travel
                    scaleX = scale
                    scaleY = scale
                    alpha = fade
                }, if (index % 2 == 0) accent else tint)
            }
        }
    }
}
