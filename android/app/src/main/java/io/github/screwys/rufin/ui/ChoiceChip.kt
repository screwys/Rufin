package io.github.screwys.rufin.ui

import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** Rufin's chip: always filled, so it reads as a box on any background, the chosen one in the
 *  primary container. Every choice, filter and link chip uses it. */
@Composable
internal fun ChoiceChip(
    selected: Boolean, onClick: () -> Unit, label: @Composable () -> Unit, modifier: Modifier = Modifier,
    enabled: Boolean = true, leadingIcon: (@Composable () -> Unit)? = null,
) {
    val scheme = MaterialTheme.colorScheme
    FilterChip(selected, onClick, label, modifier, enabled, leadingIcon = leadingIcon, border = null,
        colors = FilterChipDefaults.filterChipColors(
            containerColor = scheme.surfaceContainerHighest, labelColor = scheme.onSurface, iconColor = scheme.onSurface,
            selectedContainerColor = scheme.primaryContainer, selectedLabelColor = scheme.onPrimaryContainer,
            disabledContainerColor = scheme.surfaceContainer, disabledSelectedContainerColor = scheme.surfaceContainer))
}
