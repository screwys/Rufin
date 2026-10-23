package io.github.screwys.rufin.ui

import io.github.screwys.rufin.app.RufinConnection
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.withContext
import io.github.screwys.rufin.browse.*
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate

internal val LocalDownloadBadges = staticCompositionLocalOf<RufinConnection?> { null }

internal val LocalShowDownloadedBadges = staticCompositionLocalOf { true }

@Composable
internal fun DownloadBadge(item: BrowseItem, modifier: Modifier = Modifier) {
    val row = item.row
    if (!LocalShowDownloadedBadges.current || row.mediaUri.startsWith("file:") || row.mediaUri.startsWith("rufin:cue/")) return
    val target = row.downloadItem ?: return
    val model = LocalDownloadBadges.current
    val library = model?.browse?.library
    val revision = model?.downloads?.state?.revision
    var downloaded by remember(target, library, item.downloadRevision, row.downloaded) { mutableStateOf(row.downloaded) }
    LaunchedEffect(target, library, revision, item.downloadRevision) {
        try {
            if (library != null && item.downloadRevision != revision) {
                val status = withContext(Dispatchers.Default) { library.itemDownloadStatus(target) }
                downloaded = status.total > 0UL && status.downloaded == status.total
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model?.runAction { throw error } }
    }
    if (downloaded) RufinIcon("rufin-folder-download-symbolic", translate("Downloaded"),
        modifier.size(16.dp), MaterialTheme.colorScheme.primary)
}
