package io.github.screwys.rufin.ui

import androidx.compose.animation.ContentTransform
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween

internal fun pageMotion(forward: Boolean, reduceMotion: Boolean): ContentTransform {
    val direction = if (forward) 1 else -1
    val duration = if (reduceMotion) 0 else 300
    return (slideInHorizontally(tween(duration, easing = FastOutSlowInEasing)) { it * direction / 4 } +
        fadeIn(tween(duration * 3 / 4))) togetherWith
        (slideOutHorizontally(tween(duration, easing = FastOutSlowInEasing)) { -it * direction / 4 } +
            fadeOut(tween(duration / 4)))
}
