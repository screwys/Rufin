package io.github.screwys.rufin.ui

import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.browse.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidArtistLink
import io.github.screwys.rufin.core.AndroidPlayerMetadata
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import kotlinx.coroutines.CancellationException

/** The places a row leads to, read once: its artists, its albums and its source's link. Null while they are read. */
@Composable
internal fun rememberMediaLinks(metadata: AndroidPlayerMetadata?, player: PlayerConnection, mediaUri: String?,
    originRoute: Route?): Result<AndroidPlayerMetadata?>? {
    val bridge = player.bridge
    val loaded by produceState<Result<AndroidPlayerMetadata?>?>(metadata?.let { Result.success(it) }, metadata, mediaUri, originRoute, bridge) {
        try { value = Result.success(metadata ?: mediaUri?.let { bridge?.metadataLinks(it, originRoute) }) }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { value = Result.failure(error) }
    }
    return loaded
}

/** Opens a row's source link: its server page, or the folder its files are in. */
private fun openSourceLink(player: PlayerConnection, context: android.content.Context, metadata: AndroidPlayerMetadata,
    originRoute: Route?, then: () -> Unit) {
    val link = metadata.sourceLink ?: return
    player.model.runAction {
        openMediaUri(context, link.url ?: player.bridge?.folderUri(metadata.mediaUri, originRoute) ?: return@runAction)
        then()
    }
}

/** The player's Go to: a sheet of the playing track's artists, albums and source link. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MediaNavigationSheet(metadata: AndroidPlayerMetadata?, player: PlayerConnection,
    onNavigate: (Route, String, String?) -> Unit, onDismiss: () -> Unit,
    mediaUri: String? = metadata?.mediaUri,
    originRoute: Route? = null,
) {
    val context = LocalContext.current
    // The sheet opens once its links are read, at its full height, instead of growing from a spinner.
    val result = rememberMediaLinks(metadata, player, mediaUri, originRoute) ?: return
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        val resolved = result.getOrNull()
        val links = resolved?.artistLinks.orEmpty().map { "Artist" to it } + resolved?.albumLinks.orEmpty().map { "Album" to it }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            result.exceptionOrNull()?.let { failure -> item {
                ErrorRow(failure, translate("Close"), onDismiss)
            } }
            if (result.isSuccess && links.isEmpty() && resolved?.sourceLink == null) item {
                Text(translate("No results"), Modifier.padding(horizontal = 24.dp, vertical = 16.dp))
            }
            itemsIndexed(links, key = { index, (_, link) -> "$index:${link.route}" }) { _, (kind, link) ->
                val pixels = with(LocalDensity.current) { 40.dp.roundToPx() }
                val artwork = rememberArtwork(link.artworkIdentity, player, pixels)
                ListItem(headlineContent = { Text(link.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    supportingContent = { Text(translate(kind), style = MaterialTheme.typography.bodySmall) },
                    leadingContent = { Artwork(artwork, Modifier.size(40.dp).clip(CoverShape)) },
                    modifier = Modifier.clickable { onDismiss(); onNavigate(link.route, link.title, resolved?.sourceId) })
            }
            resolved?.let { metadata -> metadata.sourceLink?.let { link -> item {
                ListItem(headlineContent = { Text(translate(link.title)) },
                    leadingContent = { DetailLinkIcon(link.iconId, Modifier.size(40.dp)) },
                    modifier = Modifier.clickable { openSourceLink(player, context, metadata, originRoute, onDismiss) })
            } } }
        }
    }
}

/** A menu's Go to: its row's artists, albums and source link as pills that wrap onto further lines. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun MediaNavigationPills(metadata: AndroidPlayerMetadata, player: PlayerConnection, originRoute: Route?,
    onNavigate: (Route, String, String?) -> Unit, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    // Each pill's 48dp touch area already spaces the lines 8dp apart.
    FlowRow(modifier, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        metadata.artistLinks.forEach { link -> MediaLinkPill(link, CircleShape, player) { onNavigate(link.route, link.title, metadata.sourceId) } }
        metadata.albumLinks.forEach { link -> MediaLinkPill(link, CoverShape, player) { onNavigate(link.route, link.title, metadata.sourceId) } }
        metadata.sourceLink?.let { link ->
            MediaPill(translate(link.title), { openSourceLink(player, context, metadata, originRoute, onDismiss) }) {
                DetailLinkIcon(link.iconId, Modifier.size(24.dp))
            }
        }
    }
}

@Composable
private fun MediaLinkPill(link: AndroidArtistLink, shape: Shape, player: PlayerConnection, onClick: () -> Unit) {
    val artwork = rememberArtwork(link.artworkIdentity, player, with(LocalDensity.current) { 28.dp.roundToPx() })
    MediaPill(link.title, onClick) { Artwork(artwork, Modifier.size(28.dp).clip(shape)) }
}

@Composable
private fun MediaPill(title: String, onClick: () -> Unit, leading: @Composable () -> Unit) {
    // Tinted by the text color, so it shows on the menu's light and dark cover-tinted backdrops alike.
    Surface(onClick = onClick, shape = RoundedCornerShape(50), color = MaterialTheme.colorScheme.onSurface.copy(alpha = .08f),
        contentColor = MaterialTheme.colorScheme.onSurface) {
        Row(Modifier.heightIn(min = 40.dp).padding(start = 6.dp, end = 14.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            leading()
            Text(title, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}
