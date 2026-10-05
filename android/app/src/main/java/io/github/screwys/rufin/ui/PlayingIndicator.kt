package io.github.screwys.rufin.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.settings.LocalReduceMotion
import kotlinx.coroutines.isActive
import kotlin.math.PI
import kotlin.math.cos

@Composable
internal fun PlayingIndicator() {
    if (LocalReduceMotion.current) return
    var seconds by remember { mutableDoubleStateOf(0.0) }
    LaunchedEffect(Unit) {
        while (isActive) withFrameNanos { seconds = (it / 1_000_000L % 1_800L) / 1_000.0 }
    }
    val color = MaterialTheme.colorScheme.secondary
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    Canvas(Modifier.size(13.dp, 12.dp)) {
        repeat(3) { index ->
            val phase = ((seconds + index * .3) / .9) % 2
            val progress = if (phase <= 1) phase else 2 - phase
            val first = progress <= .5
            val eased = (1 - cos((if (first) progress * 2 else (progress - .5) * 2) * PI)) * .5
            val height = ((if (first) 4 else 12) + (if (first) 8 else -4) * eased).toFloat().dp.toPx()
            val column = if (rtl) 2 - index else index
            drawRoundRect(color, Offset(column * 5.dp.toPx(), size.height - height), Size(3.dp.toPx(), height), CornerRadius(1.dp.toPx()))
        }
    }
}
