package io.github.screwys.rufin.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier

@Composable
internal fun FallbackCover(modifier: Modifier = Modifier) {
    Box(modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        RufinIcon("rufin-cover-fallback-symbolic", null, Modifier.fillMaxSize(.75f), MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
