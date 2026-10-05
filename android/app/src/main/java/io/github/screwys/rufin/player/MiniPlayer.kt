package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.basicMarquee
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.*
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.spring
import androidx.compose.ui.graphics.graphicsLayer
import kotlinx.coroutines.launch
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.tween
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.LocalReduceMotion

@Composable
internal fun PlayerIconButton(
    icon: String, label: String, onClick: () -> Unit, modifier: Modifier = Modifier,
    enabled: Boolean = true, selected: Boolean = false,
    iconSize: Dp = 24.dp,
    glyph: Boolean = false,
) {
    val tint by animateColorAsState(if (selected) MaterialTheme.colorScheme.primary else LocalContentColor.current,
        animationSpec = tween(if (LocalReduceMotion.current) 0 else 180), label = "playerActionTint")
    IconButton(onClick, modifier, enabled, colors = IconButtonDefaults.iconButtonColors(contentColor = tint)) {
        RufinIcon(icon, translate(label), Modifier.size(iconSize), glyph = glyph)
    }
}

private data class MiniMedia(val uri: String, val title: String, val subtitle: String,
    val playing: Boolean, val duration: ULong)

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun MiniPlayer(player: PlayerConnection, onOpen: () -> Unit, onOutput: () -> Unit) {
    val media by remember(player) { derivedStateOf {
        player.playback?.let { current -> current.mediaUri?.let { uri ->
            MiniMedia(uri, current.title, current.artist,
                current.desiredPlaying, current.durationMillis)
        } }
    } }
    val current = media ?: return
    val reduceMotion = LocalReduceMotion.current
    val artwork = player.artwork
    val background by animateColorAsState(
        artwork?.color?.let { lerp(MaterialTheme.colorScheme.surfaceContainer, it, .24f) }
            ?: MaterialTheme.colorScheme.surfaceContainer,
        animationSpec = tween(if (reduceMotion) 0 else 320), label = "miniPlayerColor")
    val threshold = with(LocalDensity.current) { 48.dp.toPx() }
    val offset = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    Surface(
        Modifier.padding(start = 12.dp, end = 12.dp, bottom = 4.dp).widthIn(max = 600.dp).fillMaxWidth()
            .graphicsLayer { translationX = offset.value }
            .pointerInput(threshold, reduceMotion) {
                var distance = 0f
                detectHorizontalDragGestures(onDragStart = {
                    distance = offset.value
                    scope.launch { offset.stop() }
                }, onHorizontalDrag = { change, amount ->
                    change.consume()
                    distance += amount
                    scope.launch { offset.snapTo(distance) }
                }, onDragEnd = {
                    if (distance < -threshold) player.next() else if (distance > threshold) player.previous()
                    scope.launch { if (reduceMotion) offset.snapTo(0f) else offset.animateTo(0f, spring()) }
                }, onDragCancel = { scope.launch { if (reduceMotion) offset.snapTo(0f) else offset.animateTo(0f, spring()) } })
            }.pointerInput(threshold, onOpen) {
                var distance = 0f
                detectVerticalDragGestures(onDragStart = { distance = 0f },
                    onVerticalDrag = { change, amount -> change.consume(); distance += amount },
                    onDragEnd = { if (distance < -threshold) onOpen() }, onDragCancel = { distance = 0f })
            }.clickable(onClick = onOpen),
        shape = RoundedCornerShape(16.dp), color = background, contentColor = MaterialTheme.colorScheme.onSurface,
    ) {
        Column {
            Row(Modifier.heightIn(min = 68.dp).padding(horizontal = 10.dp, vertical = 10.dp),
                verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Artwork(artwork, Modifier.size(48.dp).clip(RoundedCornerShape(8.dp)))
                AnimatedContent(current.title to current.subtitle, Modifier.weight(1f).padding(start = 10.dp, end = 4.dp),
                    transitionSpec = { (slideInHorizontally(tween(if (reduceMotion) 0 else 180)) { it / 3 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
                        (slideOutHorizontally(tween(if (reduceMotion) 0 else 180)) { -it / 3 } + fadeOut(tween(if (reduceMotion) 0 else 120))) },
                    label = "miniPlayerMetadata") { metadata ->
                    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Text(metadata.first, Modifier.fillMaxWidth().basicMarquee(), maxLines = 1,
                            fontWeight = FontWeight.SemiBold, style = MaterialTheme.typography.bodyLarge)
                        Text(metadata.second, Modifier.fillMaxWidth().basicMarquee(), maxLines = 1,
                            style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                PlayerIconButton("rufin-speaker-front-symbolic", "Audio output", onOutput)
                val favorite = player.projectedFavorite(current.uri, player.playback?.favorite == true)
                PlayerIconButton(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                    "Favorite current track", player::favorite, selected = favorite)
                IconButton(player::togglePlayback) {
                    RufinIcon(if (current.playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                        translate(if (current.playing) "Pause" else "Play"), Modifier.size(24.dp))
                }
            }
            if (current.duration > 0UL) MiniProgress(player, current.duration, player.playback?.positionAdvancing == true)
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun MiniProgress(player: PlayerConnection, duration: ULong, playing: Boolean) {
    val interpolate = playing && !LocalReduceMotion.current
    var position by remember { mutableStateOf(0UL) }
    LaunchedEffect(player, duration, interpolate) {
        do {
            position = player.positionNow()
            if (interpolate) withFrameNanos { }
        } while (interpolate)
    }
    LinearProgressIndicator(
        progress = { ((if (interpolate) position else player.playback?.positionMillis ?: 0UL).toDouble() /
            duration.toDouble()).toFloat().coerceIn(0f, 1f) },
        modifier = Modifier.fillMaxWidth().height(3.dp), color = MaterialTheme.colorScheme.primary,
        trackColor = Color.Transparent,
        drawStopIndicator = {},
    )
}
