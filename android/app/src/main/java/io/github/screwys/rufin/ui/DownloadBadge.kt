package io.github.screwys.rufin.ui

import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidBrowseRow
import io.github.screwys.rufin.core.translate

internal val LocalShowDownloadedBadges = staticCompositionLocalOf { true }

@Composable
internal fun DownloadBadge(row: AndroidBrowseRow, modifier: Modifier = Modifier) {
    if (LocalShowDownloadedBadges.current && row.downloaded && !row.mediaUri.startsWith("file:") && !row.mediaUri.startsWith("rufin:cue/"))
        RufinIcon("rufin-folder-download-symbolic", translate("Downloaded"),
            modifier.size(16.dp), MaterialTheme.colorScheme.primary)
}
