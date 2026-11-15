package io.github.screwys.rufin.player

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

/** LIVE with its dot, where a radio station plays. */
@Composable
internal fun LiveMark(style: TextStyle, color: Color = LocalContentColor.current) {
    // The theme's error colour, and a red that reads on a dark surface.
    val dot = if (MaterialTheme.colorScheme.surface.luminance() < .5f) Color(0xFFF66151) else MaterialTheme.colorScheme.error
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Box(Modifier.size(7.dp).clip(CircleShape).background(dot))
        Text(translate("LIVE"), style = style, color = color, maxLines = 1)
    }
}
