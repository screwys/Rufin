package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

/** A tool beside a page's content, as the player's: a small tonal square with its icon. */
@Composable
internal fun ToolButton(icon: String, label: String, enabled: Boolean = true, action: () -> Unit) {
    FilledTonalIconButton(action, Modifier.padding(horizontal = 2.dp).size(36.dp).focusRing(RoundedCornerShape(10.dp)), enabled, RoundedCornerShape(10.dp),
        IconButtonDefaults.filledTonalIconButtonColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            contentColor = MaterialTheme.colorScheme.onSurface)) {
        RufinIcon(icon, translate(label), Modifier.size(18.dp), glyph = true)
    }
}
