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
    // On a canvas wider than the 16 px grid the body is the grid's size: drawn that much larger, it matches
    // the icons beside it, and what the canvas holds around it (a sparkle, a stem) reaches past the box.
    } else if (resource.canvas != 1f) modifier.graphicsLayer { scaleX = resource.canvas; scaleY = resource.canvas }
    else modifier
    Icon(painterResource(resource.drawable), contentDescription, placement, tint)
}
