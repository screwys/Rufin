package io.github.screwys.rufin.downloads

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.LoadState
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidDownloadJob
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.RufinIcon

@Composable
internal fun DownloadsPage(model: RufinConnection, player: PlayerConnection, onSettings: () -> Unit) {
    val downloads = model.downloads
    DisposableEffect(downloads) { downloads.queueVisible = true; onDispose { downloads.queueVisible = false } }
    val state = downloads.state
    val jobs = downloads.pages.collectAsLazyPagingItems()
    var cancel by remember { mutableStateOf<AndroidDownloadJob?>(null) }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(trackCountText(state?.queues?.sumOf { it.downloadedTracks } ?: 0UL),
                Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
            IconButton({ downloads.pause(state?.paused != true) }, enabled = state?.queues?.any { it.totalJobs > 0UL } == true) {
                RufinIcon(if (state?.paused == true) "rufin-media-playback-start-symbolic" else "rufin-media-playback-pause-symbolic",
                    translate(if (state?.paused == true) "Resume" else "Pause"))
            }
            PlayerIconButton("rufin-preferences-system-symbolic", "Settings", onSettings)
        }
        state?.notice?.let { Text(it, Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.bodySmall) }
        LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(bottom = 16.dp + LocalPlayerBottomPadding.current)) {
            items(jobs.itemCount, key = jobs.itemKey { it.id }) { index ->
                jobs[index]?.let { job ->
                    val art = rememberArtwork(job.artworkIdentity, player, with(LocalDensity.current) { 56.dp.roundToPx() })
                    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            Artwork(art, Modifier.size(56.dp).clip(RoundedCornerShape(8.dp)))
                            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                                Text(translate(job.title), maxLines = 2, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleSmall)
                                if (job.subtitle.isNotBlank()) Text(job.subtitle, maxLines = 2, overflow = TextOverflow.Ellipsis,
                                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                Text("${job.completed}/${job.total} · ${translate(job.state)}", style = MaterialTheme.typography.bodySmall)
                            }
                            PlayerIconButton("rufin-window-close-symbolic", "Cancel", { cancel = job })
                        }
                        LinearProgressIndicator(progress = { if (job.total == 0UL) 0f else job.completed.toFloat() / job.total.toFloat() },
                            modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
                    }
                }
            }
            if (jobs.itemCount == 0 && jobs.loadState.refresh is LoadState.NotLoading) item {
                Column(Modifier.fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    RufinIcon("rufin-object-select-symbolic", null, Modifier.size(64.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    Text(translate("No downloads queued"), color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            if (jobs.loadState.refresh is LoadState.Loading || jobs.loadState.append is LoadState.Loading) item {
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            val error = (jobs.loadState.refresh as? LoadState.Error)?.error ?: (jobs.loadState.append as? LoadState.Error)?.error
            error?.let { item { ErrorRow(it, translate("Retry"), jobs::retry) } }
        }
    }
    cancel?.let { job -> AlertDialog(onDismissRequest = { cancel = null }, title = { Text(translate("Cancel download")) },
        text = { Text(translate("Completed downloads will stay on this device.")) },
        confirmButton = { TextButton({ downloads.cancel(job); cancel = null }) { Text(translate("Cancel download")) } },
        dismissButton = { TextButton({ cancel = null }) { Text(translate("Continue")) } }) }
}
