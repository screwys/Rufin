package io.github.screwys.rufin.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidPlayerMetadata
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import kotlinx.coroutines.CancellationException

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MediaNavigationSheet(metadata: AndroidPlayerMetadata?, player: PlayerConnection,
    onNavigate: (String, String, String?) -> Unit, onDismiss: () -> Unit,
    mediaUri: String? = metadata?.mediaUri,
    originRoute: String? = null,
) {
    val bridge = player.bridge
    val loaded by produceState<Result<AndroidPlayerMetadata?>?>(metadata?.let { Result.success(it) }, metadata, mediaUri, originRoute, bridge) {
        try { value = Result.success(metadata ?: mediaUri?.let { bridge?.metadataLinks(it, originRoute) }) }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { value = Result.failure(error) }
    }
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        val resolved = loaded?.getOrNull()
        val links = resolved?.artistLinks.orEmpty().map { "Artist" to it } + resolved?.albumLinks.orEmpty().map { "Album" to it }
        ArtworkReadyContent(key = resolved?.mediaUri, modifier = Modifier.fillMaxWidth()) {
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            if (loaded == null) item {
                Box(Modifier.fillMaxWidth().height(96.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
            else loaded?.exceptionOrNull()?.let { failure -> item {
                ErrorRow(failure, translate("Close"), onDismiss)
            } }
            if (loaded?.isSuccess == true && links.isEmpty()) item {
                Text(translate("No results"), Modifier.padding(horizontal = 24.dp, vertical = 16.dp))
            }
            itemsIndexed(links, key = { index, (_, link) -> "$index:${link.route}" }) { _, (kind, link) ->
                val pixels = with(LocalDensity.current) { 40.dp.roundToPx() }
                val artwork = rememberArtwork(link.artworkIdentity, player, pixels)
                ListItem(headlineContent = { Text(link.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    supportingContent = { Text(translate(kind), style = MaterialTheme.typography.bodySmall) },
                    leadingContent = { Artwork(artwork, Modifier.size(40.dp).clip(RoundedCornerShape(8.dp))) },
                    modifier = Modifier.clickable { onDismiss(); onNavigate(link.route, link.title, resolved?.sourceId) })
            }
        }
        }
    }
}
