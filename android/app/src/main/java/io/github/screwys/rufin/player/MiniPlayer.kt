package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.focusRing

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.basicMarquee
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import io.github.screwys.rufin.ui.CoverShape
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
import androidx.compose.animation.Crossfade
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.LocalReduceMotion

/** The size of the icons in a surface's bars and tools: a tablet's page bars, sidebar and queue pane share
 *  one, smaller than a phone's. */
internal val LocalToolIconSize = compositionLocalOf { 24.dp }

@Composable
internal fun PlayerIconButton(
    icon: String, label: String, onClick: () -> Unit, modifier: Modifier = Modifier,
    enabled: Boolean = true, selected: Boolean = false,
    iconSize: Dp = LocalToolIconSize.current,
    glyph: Boolean = false,
) {
    val tint by animateColorAsState(if (selected) MaterialTheme.colorScheme.primary else LocalContentColor.current,
        animationSpec = tween(if (LocalReduceMotion.current) 0 else 180), label = "playerActionTint")
    IconButton(onClick, modifier.focusRing(androidx.compose.foundation.shape.CircleShape), enabled, colors = IconButtonDefaults.iconButtonColors(contentColor = tint)) {
        RufinIcon(icon, translate(label), Modifier.size(iconSize), glyph = glyph)
    }
}

private data class MiniMedia(val uri: String, val title: String, val subtitle: String,
    val playing: Boolean, val duration: ULong)

@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun MiniPlayer(player: PlayerConnection, onOpen: () -> Unit, onOutput: () -> Unit,
    modifier: Modifier = Modifier.padding(start = 8.dp, end = 8.dp, bottom = 4.dp), compact: Boolean = false,
    /** A drag up follows the finger into the player: each move, then whether it opened. */
    onReveal: ((Float) -> Unit)? = null, onRevealEnd: ((Boolean) -> Unit)? = null,
    /** A tablet's floats over the page, centred. */
    floating: Boolean = false,
) {
    val media by remember(player) { derivedStateOf {
        player.playback?.let { current -> current.transport.current?.id?.mediaUri?.let { uri ->
            MiniMedia(uri, player.currentItem?.metadata?.title.orEmpty(), player.currentItem?.metadata?.artist.orEmpty(),
                current.transport.desiredPlaying, current.transport.durationMillis)
        } }
    } }
    val current = media ?: return
    // On foldable and tablet widths the bar holds the transport: output, previous, Play in the accent, next, favorite.
    val tablet = floating
    val foldable = !compact && androidx.compose.ui.platform.LocalConfiguration.current.screenWidthDp >= 600
    val reduceMotion = LocalReduceMotion.current
    val artwork = rememberArtwork(player.currentItem?.artworkBinding, player, with(LocalDensity.current) { 48.dp.roundToPx() })
    val background by animateColorAsState(
        artwork.image?.color?.let { lerp(MaterialTheme.colorScheme.surfaceContainer, it, .24f) }
            ?: MaterialTheme.colorScheme.surfaceContainer,
        animationSpec = tween(if (reduceMotion) 0 else 320), label = "miniPlayerColor")
    val threshold = with(LocalDensity.current) { 48.dp.toPx() }
    val (swipe, offset) = rememberTrackSwipe(player)
    Surface(
        modifier.fillMaxWidth()
            .then(swipe)
            .pointerInput(threshold, onOpen, onReveal) {
                var distance = 0f
                detectVerticalDragGestures(onDragStart = { distance = 0f },
                    onVerticalDrag = { change, amount -> change.consume(); distance += amount; onReveal?.invoke(amount) },
                    onDragEnd = { onRevealEnd?.invoke(distance < -threshold) ?: run { if (distance < -threshold) onOpen() } },
                    onDragCancel = { distance = 0f; onRevealEnd?.invoke(false) })
            }.clickable(interactionSource = null, indication = null, onClick = onOpen),
        shape = RoundedCornerShape(if (compact) 28.dp else if (tablet) 22.dp else 16.dp), color = background,
        contentColor = MaterialTheme.colorScheme.onSurface, shadowElevation = if (tablet) 8.dp else 0.dp,
    ) {
        Column {
            // The bar hugs its 48 dp cover; docked, a 40 dp cover keeps clear of the pill's rounded ends.
            Row(Modifier.heightIn(min = if (tablet) 68.dp else 56.dp).padding(horizontal = if (tablet) 10.dp else 8.dp, vertical = 4.dp),
                verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                // The cover stays and cross-fades; the title slides with the finger inside its own faded area.
                CoverCrossfade(artwork, Modifier.size(if (compact) 40.dp else if (tablet) 52.dp else 48.dp))
                AnimatedContent(current.title to current.subtitle, Modifier.weight(1f).padding(start = 10.dp, end = 4.dp)
                    .trackSlide(offset.value),
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
                // Folded into the dock, the mini player keeps only Play.
                if (!compact && !foldable) {
                    PlayerIconButton("rufin-speaker-front-symbolic", "Audio output", onOutput)
                    val favorite = player.favorite
                    PlayerIconButton(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                        "Favorite current track", player::favorite, selected = favorite)
                }
                if (foldable) {
                    PlayerIconButton("rufin-speaker-front-symbolic", "Audio output", onOutput)
                    PlayerIconButton("rufin-media-skip-backward-symbolic", "Previous Track", player::previous)
                    BarPlay(player, current.playing)
                    PlayerIconButton("rufin-media-skip-forward-symbolic", "Next Track", player::next)
                    val favorite = player.favorite
                    PlayerIconButton(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                        "Favorite current track", player::favorite, selected = favorite)
                } else IconButton(player::togglePlayback) {
                    if (player.refilling) RefillSpinner(34.dp)
                    RufinIcon(if (current.playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                        translate(if (current.playing) "Pause" else "Play"), Modifier.size(24.dp))
                }
            }
            if (current.duration > 0UL) MiniProgress(player, current.duration, player.playback?.transport?.state == io.github.screwys.rufin.shared.playback.TransportStatus.PLAYING)
        }
    }
}

/** A player bar's cover, cross-fading to the next track's. */
@Composable
private fun CoverCrossfade(artwork: io.github.screwys.rufin.ui.Cover, modifier: Modifier) {
    Crossfade(artwork, modifier.clip(CoverShape), animationSpec = tween(if (LocalReduceMotion.current) 0 else 220),
        label = "playerBarCover") { shown -> Artwork(shown, Modifier.fillMaxSize()) }
}

/** The title and artist follow a swipe inside their own area, fading out at its edges. */
private fun Modifier.trackSlide(offset: Float): Modifier = clipToBounds()
    .graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen }
    .drawWithContent {
        val fade = 12.dp.toPx() / size.width.coerceAtLeast(1f)
        drawContent()
        drawRect(androidx.compose.ui.graphics.Brush.horizontalGradient(0f to Color.Transparent, fade to Color.Black,
            1f - fade to Color.Black, 1f to Color.Transparent), blendMode = androidx.compose.ui.graphics.BlendMode.DstIn)
    }
    .graphicsLayer { translationX = offset }

/** A horizontal swipe over a player bar: the gesture for the whole bar, and the offset the track
 *  inside follows; past 48 dp it changes to the next or previous track. */
@Composable
private fun rememberTrackSwipe(player: PlayerConnection): Pair<Modifier, Animatable<Float, androidx.compose.animation.core.AnimationVector1D>> {
    val reduceMotion = LocalReduceMotion.current
    val threshold = with(LocalDensity.current) { 48.dp.toPx() }
    val offset = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    val gesture = Modifier.pointerInput(player, threshold, reduceMotion) {
        var distance = 0f
        fun settle() { scope.launch { if (reduceMotion) offset.snapTo(0f) else offset.animateTo(0f, spring()) } }
        detectHorizontalDragGestures(onDragStart = {
            distance = offset.value
            scope.launch { offset.stop() }
        }, onHorizontalDrag = { change, amount ->
            change.consume()
            distance += amount
            scope.launch { offset.snapTo(distance) }
        }, onDragEnd = {
            if (distance < -threshold) player.next() else if (distance > threshold) player.previous()
            settle()
        }, onDragCancel = { settle() })
    }
    return gesture to offset
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
        progress = { ((if (interpolate) position else player.playback?.transport?.positionMillis ?: 0UL).toDouble() /
            duration.toDouble()).toFloat().coerceIn(0f, 1f) },
        modifier = Modifier.fillMaxWidth().height(3.dp).graphicsLayer(), color = MaterialTheme.colorScheme.primary,
        trackColor = Color.Transparent,
        drawStopIndicator = {},
    )
}

/** The playing track on a TV, in the page's bar: it opens the player, which holds the transport. */
@Composable
internal fun NowPlayingChip(player: PlayerConnection, onOpen: () -> Unit) {
    val metadata = player.currentItem?.metadata ?: return
    val playing = player.playback?.transport?.desiredPlaying == true
    val artwork = rememberArtwork(player.currentItem?.artworkBinding, player, with(LocalDensity.current) { 32.dp.roundToPx() })
    Surface(onOpen, Modifier.padding(end = 8.dp).focusRing(RoundedCornerShape(24.dp)), shape = RoundedCornerShape(24.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHigh) {
        Row(Modifier.height(48.dp).padding(start = 8.dp, end = 14.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            CoverCrossfade(artwork, Modifier.size(32.dp))
            Column(Modifier.widthIn(max = 180.dp)) {
                Text(metadata.title, maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    fontWeight = FontWeight.SemiBold, style = MaterialTheme.typography.bodyMedium)
                Text(metadata.artist, maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            RufinIcon(if (playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic", null, Modifier.size(18.dp))
        }
    }
}

/** Play on the foldable's and the tablet's players: the accent circle, the refill ring around it. */
@Composable
private fun BarPlay(player: PlayerConnection, playing: Boolean) {
    Box(Modifier.size(48.dp), contentAlignment = androidx.compose.ui.Alignment.Center) {
        if (player.refilling) RefillSpinner(50.dp, MaterialTheme.colorScheme.primary)
        FilledIconButton(player::togglePlayback, Modifier.size(44.dp).focusRing(androidx.compose.foundation.shape.CircleShape)) {
            RufinIcon(if (playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                translate(if (playing) "Pause" else "Play"), Modifier.size(22.dp))
        }
    }
}

/** The player at the foot of a phone's landscape sidebar, as Spotify places it: the track, then
 *  output, play and next under it, and the progress line. */
@Composable
internal fun SidebarPlayer(player: PlayerConnection, onOpen: () -> Unit, onOutput: () -> Unit, modifier: Modifier = Modifier) {
    val playing by remember(player) { derivedStateOf { player.playback?.transport?.desiredPlaying == true } }
    val duration = player.playback?.transport?.durationMillis ?: 0UL
    val artwork = rememberArtwork(player.currentItem?.artworkBinding, player, with(LocalDensity.current) { 44.dp.roundToPx() })
    // Only Next shows; a swipe goes either way, as the mini player's does.
    val (swipe, offset) = rememberTrackSwipe(player)
    Surface(modifier.fillMaxWidth().then(swipe).clickable(interactionSource = null, indication = null, onClick = onOpen), shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceContainer, contentColor = MaterialTheme.colorScheme.onSurface) {
        Column(Modifier.padding(start = 8.dp, end = 4.dp, top = 8.dp, bottom = 4.dp)) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                CoverCrossfade(artwork, Modifier.size(44.dp))
                Column(Modifier.weight(1f).padding(start = 10.dp).trackSlide(offset.value)) {
                    Text(player.currentItem?.metadata?.title.orEmpty(), Modifier.basicMarquee(), maxLines = 1, fontWeight = FontWeight.SemiBold,
                        style = MaterialTheme.typography.bodyMedium)
                    Text(player.currentItem?.metadata?.artist.orEmpty(), Modifier.basicMarquee(), maxLines = 1,
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                val favorite = player.favorite
                PlayerIconButton(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                    "Favorite current track", player::favorite, selected = favorite)
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly, verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                PlayerIconButton("rufin-speaker-front-symbolic", "Audio output", onOutput)
                FilledIconButton(player::togglePlayback, Modifier.size(44.dp)) {
                    RufinIcon(if (playing) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                        translate(if (playing) "Pause" else "Play"), Modifier.size(22.dp))
                }
                PlayerIconButton("rufin-media-skip-forward-symbolic", "Next Track", player::next)
            }
            if (duration > 0UL) Box(Modifier.padding(end = 4.dp, bottom = 2.dp)) {
                MiniProgress(player, duration, player.playback?.transport?.state == io.github.screwys.rufin.shared.playback.TransportStatus.PLAYING)
            }
        }
    }
}
