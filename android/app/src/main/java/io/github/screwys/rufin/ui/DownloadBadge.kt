package io.github.screwys.rufin.ui

import io.github.screwys.rufin.browse.*
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

internal val LocalShowDownloadedBadges = staticCompositionLocalOf { true }

/** The downloaded badge of a row, as `item` shows its downloads. */
@Composable
internal fun DownloadBadge(item: BrowseItem, modifier: Modifier = Modifier) {
    val row = item.row
    if (!LocalShowDownloadedBadges.current || row.mediaUri.startsWith("file:") || row.mediaUri.startsWith("rufin:cue/")) return
    if (row.downloaded) RufinIcon("rufin-folder-download-symbolic", translate("Downloaded"),
        modifier.size(16.dp), MaterialTheme.colorScheme.primary)
}
