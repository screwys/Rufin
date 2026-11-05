package io.github.screwys.rufin.ui

import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.browse.*
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

internal val LocalDownloadBadges = staticCompositionLocalOf<RufinConnection?> { null }

internal val LocalShowDownloadedBadges = staticCompositionLocalOf { true }

/** The downloaded badge of a row, as the visible page shows its downloads. */
@Composable
internal fun DownloadBadge(item: BrowseItem, modifier: Modifier = Modifier) {
    val row = item.row
    if (!LocalShowDownloadedBadges.current || row.mediaUri.startsWith("file:") || row.mediaUri.startsWith("rufin:cue/")) return
    val shown = LocalDownloadBadges.current?.let { favoriteItem(it, item) } ?: item
    if (shown.row.downloaded) RufinIcon("rufin-folder-download-symbolic", translate("Downloaded"),
        modifier.size(16.dp), MaterialTheme.colorScheme.primary)
}
