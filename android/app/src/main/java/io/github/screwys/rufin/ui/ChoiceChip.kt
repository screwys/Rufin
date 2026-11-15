package io.github.screwys.rufin.ui

import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import io.github.screwys.rufin.browse.OnBand

/** Rufin's chip: always filled, so it reads as a box on any background, the chosen one in the
 *  primary container. Every choice, filter and link chip uses it; `onBand` is its look on a page's band. */
@Composable
internal fun ChoiceChip(
    selected: Boolean, onClick: () -> Unit, label: @Composable () -> Unit, modifier: Modifier = Modifier,
    enabled: Boolean = true, leadingIcon: (@Composable () -> Unit)? = null, onBand: Boolean = false,
) {
    val scheme = MaterialTheme.colorScheme
    val content = if (onBand) OnBand else scheme.onSurface
    FilterChip(selected, onClick, label, modifier, enabled, leadingIcon = leadingIcon, border = null,
        colors = FilterChipDefaults.filterChipColors(
            containerColor = if (onBand) Color.White.copy(alpha = .17f) else scheme.surfaceContainerHighest,
            labelColor = content, iconColor = content,
            selectedContainerColor = scheme.primaryContainer, selectedLabelColor = scheme.onPrimaryContainer,
            disabledContainerColor = scheme.surfaceContainer, disabledSelectedContainerColor = scheme.surfaceContainer))
}
