package io.github.screwys.rufin.settings

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidThemeOption
import io.github.screwys.rufin.ui.RufinIcon
import kotlin.math.min

@Composable
internal fun ThemePreview(theme: AndroidThemeOption?, dark: Boolean, systemAccent: Boolean, modifier: Modifier = Modifier) {
    val defaults = themeDefaults(dark, systemAccent)
    val scheme = remember(theme?.colors, defaults, systemAccent) {
        resolveThemeScheme(ThemeColors(theme?.colors.orEmpty()), defaults, systemAccent)
    }
    val muted = scheme.onSurface.copy(alpha = .28f)
    val cover = scheme.onSurface.copy(alpha = .12f)
    Column(modifier.clip(RoundedCornerShape(8.dp)).background(scheme.background).clearAndSetSemantics { }) {
        Row(Modifier.fillMaxWidth().height(16.dp).padding(horizontal = 6.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Box(Modifier.width(26.dp).height(2.dp).background(muted, RoundedCornerShape(1.dp)))
            RufinIcon("rufin-search-symbolic", null, Modifier.size(7.dp), scheme.onSurface)
        }
        Canvas(Modifier.fillMaxWidth().weight(1f).background(scheme.surface)) {
            val gap = size.width * .065f
            val featureHeight = size.height * .25f
            val radius = CornerRadius(gap * .3f)
            drawRoundRect(scheme.primary.copy(alpha = .08f).compositeOver(scheme.surface),
                Offset(gap, gap), Size(size.width - gap * 2, featureHeight), radius)
            val featureCover = featureHeight - gap
            drawRoundRect(cover, Offset(gap * 1.5f, gap * 1.5f), Size(featureCover, featureCover), radius)
            val textX = gap * 2 + featureCover
            val textWidth = (size.width - textX - gap * 2).coerceAtLeast(0f)
            drawRoundRect(muted, Offset(textX, gap + featureHeight * .35f), Size(textWidth, gap * .22f), radius)
            drawRoundRect(muted, Offset(textX, gap + featureHeight * .6f), Size(textWidth * .7f, gap * .22f), radius)
            val headingY = featureHeight + gap * 2.5f
            drawRoundRect(muted, Offset(gap, headingY), Size(size.width * .3f, gap * .25f), radius)
            val gridY = headingY + gap
            val side = min((size.width - gap * 4) / 3, (size.height - gridY - gap * 3) / 2).coerceAtLeast(0f)
            repeat(2) { row -> repeat(3) { column ->
                val x = gap + column * (side + gap)
                val y = gridY + row * (side + gap * 1.5f)
                drawRoundRect(cover, Offset(x, y), Size(side, side), radius)
                drawRoundRect(muted, Offset(x, y + side + gap * .35f), Size(side * .75f, gap * .2f), radius)
            } }
        }
        Row(Modifier.fillMaxWidth().height(24.dp).background(scheme.surfaceContainer).padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Box(Modifier.size(15.dp).background(cover, RoundedCornerShape(2.dp)))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Box(Modifier.fillMaxWidth(.9f).height(2.dp).background(muted))
                Box(Modifier.fillMaxWidth(.6f).height(1.dp).background(muted))
            }
            RufinIcon("rufin-media-playback-start-symbolic", null, Modifier.size(9.dp), scheme.onSurface)
        }
        Spacer(Modifier.fillMaxWidth().height(24.dp))
    }
}
