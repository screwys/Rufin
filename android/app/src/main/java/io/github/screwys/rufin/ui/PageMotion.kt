package io.github.screwys.rufin.ui

import androidx.compose.animation.ContentTransform
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.AnimationVector1D
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.pager.PagerState
import kotlin.math.abs

internal fun pageMotion(forward: Boolean, reduceMotion: Boolean): ContentTransform {
    val direction = if (forward) 1 else -1
    val duration = if (reduceMotion) 0 else 300
    return (slideInHorizontally(tween(duration, easing = FastOutSlowInEasing)) { it * direction / 4 } +
        fadeIn(tween(duration * 3 / 4))) togetherWith
        (slideOutHorizontally(tween(duration, easing = FastOutSlowInEasing)) { -it * direction / 4 } +
            fadeOut(tween(duration / 4)))
}

/** Shows `page` as its tab asks: a neighbour slides in, and a page further away fades in where the
 *  pager is, so the pages between never pass by. `fade` is the pager's alpha. */
internal suspend fun PagerState.showPage(page: Int, fade: Animatable<Float, AnimationVector1D>, reduceMotion: Boolean) {
    when {
        reduceMotion -> scrollToPage(page)
        abs(page - currentPage) <= 1 -> animateScrollToPage(page)
        else -> {
            fade.animateTo(0f, tween(90))
            scrollToPage(page)
            fade.animateTo(1f, tween(160))
        }
    }
}
