package io.github.screwys.rufin.ui

import io.github.screwys.rufin.rufinIconResources

import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp

@Composable
internal fun RufinIcon(
    name: String,
    contentDescription: String?,
    modifier: Modifier = Modifier.size(24.dp),
    tint: Color = LocalContentColor.current,
    glyph: Boolean = false,
) {
    val resource = rufinIconResources.getValue(name)
    val placement = if (glyph) modifier.graphicsLayer {
        scaleX = resource.glyphScale
        scaleY = resource.glyphScale
        translationX = resource.glyphX * size.width
        translationY = resource.glyphY * size.height
    } else modifier
    Icon(painterResource(resource.drawable), contentDescription, placement, tint)
}
