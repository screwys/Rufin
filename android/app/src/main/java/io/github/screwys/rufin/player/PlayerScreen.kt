package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.MediaNavigationSheet
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.PreferencesConnection

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import io.github.screwys.rufin.ui.rufinMarquee
import androidx.compose.foundation.clickable
import androidx.compose.foundation.Image
import androidx.compose.foundation.Canvas
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.background
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.offset
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.style.TextOverflow
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidTransportState
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.seekPreviewMatchesPosition
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.trackCountText
import androidx.compose.animation.Crossfade
import androidx.compose.animation.ExperimentalAnimationApi
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.updateTransition
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.ui.graphics.graphicsLayer
import kotlinx.coroutines.launch

private enum class PlayerSurface { LYRICS, VISUALIZER }

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PlayerScreen(player: PlayerConnection, preferences: PreferencesConnection, onNavigate: (String, String, String?) -> Unit, onMore: () -> Unit, onClose: () -> Unit) {
    val reduceMotion = LocalReduceMotion.current
    val scope = rememberCoroutineScope()
    var surface by remember { mutableStateOf<PlayerSurface?>(null) }
    var output by remember { mutableStateOf(false) }
    var queue by remember { mutableStateOf(false) }
    var configuration by remember { mutableStateOf<PlayerSettingsPage?>(null) }
    var navigation by remember { mutableStateOf<String?>(null) }
    val lyricsScroll = rememberLazyListState()
    val application = preferences.applicationSettings
    val lyricsEnabled = application?.optBoolean("fullscreen_lyrics_visible", true) != false
    val currentLineEnabled = application?.optBoolean("fullscreen_current_lyrics_line_visible", true) != false
    val visualizerEnabled = application?.optBoolean("fullscreen_visualizer_visible") == true
    val combined = lyricsEnabled && visualizerEnabled && application?.optBoolean("right_panel_combined") == true
    val background by animateColorAsState(
        if (application?.optBoolean("fullscreen_dynamic_background") == true)
            lerp(MaterialTheme.colorScheme.surface, player.artwork?.color ?: MaterialTheme.colorScheme.surface, .12f)
        else MaterialTheme.colorScheme.surface, tween(if (reduceMotion) 0 else 320), label = "fullPlayerColor")
    val verticalOffset = remember { Animatable(0f) }
    val trackOffset = remember { Animatable(0f) }
    val threshold = with(LocalDensity.current) { 48.dp.toPx() }
    LaunchedEffect(player) { player.refreshSettings() }
    LaunchedEffect(lyricsEnabled, visualizerEnabled) {
        if (surface == PlayerSurface.LYRICS && !lyricsEnabled || surface == PlayerSurface.VISUALIZER && !visualizerEnabled) surface = null
    }
    LaunchedEffect(surface, combined) { player.visualizer(surface == PlayerSurface.VISUALIZER || surface == PlayerSurface.LYRICS && combined) }
    DisposableEffect(player) { onDispose { player.visualizer(false) } }
    BackHandler { if (surface != null) surface = null else onClose() }
    Surface(Modifier.fillMaxSize().graphicsLayer { translationY = verticalOffset.value }
        .pointerInput(surface, threshold, reduceMotion) {
            if (surface != null) return@pointerInput
            var distance = 0f
            detectVerticalDragGestures(onDragStart = { distance = 0f }, onVerticalDrag = { change, amount ->
                if (surface != null) return@detectVerticalDragGestures
                distance = (distance + amount).coerceAtLeast(0f)
                change.consume()
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { verticalOffset.snapTo(distance) }
            }, onDragEnd = {
                if (distance > threshold) onClose()
                else scope.launch { verticalOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) }
            }, onDragCancel = { scope.launch { verticalOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) } })
        }.pointerInput(threshold, reduceMotion) {
            var distance = 0f
            detectHorizontalDragGestures(onDragStart = {
                distance = 0f
                scope.launch { trackOffset.stop() }
            }, onHorizontalDrag = { change, amount ->
                change.consume()
                distance += amount
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { trackOffset.snapTo(distance) }
            }, onDragEnd = {
                if (distance < -threshold && player.playback?.canNext == true) player.next()
                else if (distance > threshold && player.playback?.canPrevious == true) player.previous()
                scope.launch { trackOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) }
            }, onDragCancel = { scope.launch { trackOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) } })
        }, color = background, contentColor = MaterialTheme.colorScheme.onSurface) {
        Box(Modifier.fillMaxSize()) {
            if (application?.optBoolean("fullscreen_background_image") == true) player.artwork?.let { artwork ->
                Image(artwork.bitmap.asImageBitmap(), null, Modifier.matchParentSize().blur(48.dp).alpha(.12f), contentScale = ContentScale.Crop)
            }
            AnimatedContent(surface, Modifier.fillMaxSize(), transitionSpec = {
                (if (targetState == null) fadeIn(tween(if (reduceMotion) 0 else 180))
                else slideInVertically(tween(if (reduceMotion) 0 else 240)) { it / 8 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
                    fadeOut(tween(if (reduceMotion) 0 else 120))
            }, label = "playerSurface") { visible ->
                Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                        PlayerIconButton("rufin-go-down-symbolic", "Close", { if (visible == null) onClose() else surface = null })
                        if (visible == null) PlayerContext(player, Modifier.weight(1f).padding(horizontal = 8.dp))
                        else Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                            Text(player.playback?.title.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                            Text(player.playback?.artist.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                        }
                        if (visible == null) PlayerIconButton("rufin-view-more-symbolic", "More actions", onMore)
                        else if (visible == PlayerSurface.LYRICS) LyricsActions(player)
                        else Spacer(Modifier.size(48.dp))
                    }
                    if (visible == null) {
                        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
                            val viewportHeight = maxHeight
                            val content: @Composable (Modifier) -> Unit = { MainPlayerArtwork(player, currentLineEnabled, it.clipToBounds().graphicsLayer { translationX = trackOffset.value }) }
                            val controls: @Composable () -> Unit = {
                                PlayerMetadata(player, Modifier.fillMaxWidth().padding(vertical = 12.dp)) { navigation = it }
                                PlayerControls(player, Modifier.fillMaxWidth())
                            }
                            if (maxWidth > maxHeight) Row(Modifier.fillMaxSize().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                                content(Modifier.weight(1f).fillMaxHeight())
                                Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), horizontalAlignment = Alignment.CenterHorizontally) { controls() }
                            } else if (maxHeight >= 440.dp * LocalDensity.current.fontScale.coerceAtLeast(1f)) {
                                Column(Modifier.fillMaxSize().padding(horizontal = 16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                                    content(Modifier.weight(1f).fillMaxWidth())
                                    controls()
                                }
                            } else Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
                                horizontalAlignment = Alignment.CenterHorizontally) {
                                content(Modifier.fillMaxWidth().height(viewportHeight * .5f))
                                controls()
                            }
                        }
                        PlayerActions({ output = true }, { configuration = PlayerSettingsPage.PREFERENCES },
                            { queue = true }, { surface = PlayerSurface.LYRICS }, { surface = PlayerSurface.VISUALIZER },
                            lyricsEnabled, visualizerEnabled, combined)
                    } else {
                        Box(Modifier.weight(1f).fillMaxWidth().padding(top = 8.dp, bottom = 20.dp)
                            .clipToBounds().graphicsLayer { translationX = trackOffset.value }) {
                            if (visible == PlayerSurface.VISUALIZER || combined) {
                                val opacity = player.settings?.visualizer?.let { org.json.JSONObject(it).getJSONObject("appearance").getDouble("fullscreen_lyrics_opacity").toFloat() } ?: 1f
                                VisualizerView(player, Modifier.matchParentSize(), if (combined) opacity else 1f)
                            }
                            if (visible == PlayerSurface.LYRICS) LyricsPanel(player, lyricsScroll)
                        }
                        Box(Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(top = 8.dp)) { PlayerProgress(player) }
                        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(top = 16.dp, bottom = 24.dp), verticalAlignment = Alignment.CenterVertically) {
                            PlayerFooterButton("rufin-speaker-front-symbolic", "Output", { output = true })
                            Spacer(Modifier.weight(1f))
                            PlayerPlayPause(player)
                            Spacer(Modifier.weight(1f))
                            PlayerFooterButton("rufin-mixer-sliders-symbolic", "Preferences", { configuration = PlayerSettingsPage.PREFERENCES })
                        }
                    }
                }
            }
        }
    }
    if (queue) PlayerQueueSheet(player, preferences, onDismiss = { queue = false },
        onNavigate = { route, title, source -> queue = false; onNavigate(route, title, source) })
    if (output) PlayerOutputSheet(player) { output = false }
    navigation?.let { uri -> MediaNavigationSheet(player.metadataLinks?.takeIf { it.mediaUri == uri }, player,
        mediaUri = uri, onNavigate = { route, title, source -> navigation = null; onNavigate(route, title, source) },
        onDismiss = { navigation = null }) }
    configuration?.let { PlayerSettingsSheet(player, preferences, it) { configuration = null } }
}

@Composable
private fun MainPlayerArtwork(player: PlayerConnection, showCurrentLine: Boolean, modifier: Modifier) {
    val reduceMotion = LocalReduceMotion.current
    val hasLyrics = showCurrentLine && player.currentLyrics?.document?.lines?.isNotEmpty() == true
    val space by animateDpAsState(if (hasLyrics) 52.dp else 0.dp, tween(if (reduceMotion) 0 else 260), label = "lyricsPreviewSpace")
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            PlayerCover(player, Modifier.size(minOf(maxWidth, maxHeight)))
        }
        Box(Modifier.fillMaxWidth().height(space), contentAlignment = Alignment.CenterStart) {
            if (showCurrentLine) CurrentLyricsPreview(player)
        }
    }
}

@Composable
private fun PlayerContext(player: PlayerConnection, modifier: Modifier) {
    val context by remember(player) { derivedStateOf { player.playback?.let { Triple(it.contextTitle, it.contextKind, it.contextCategory) } } }
    val title = context?.first ?: when (context?.second) {
        "manual" -> translate("Queue")
        "random" -> translate("Play random")
        "radio" -> translate("Radio")
        "auto_dj" -> translate("Auto DJ")
        else -> null
    }
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        title?.let {
            val heading = context?.third?.let { category -> translate("Playing from {}").replace("{}", translate(category)) }
                ?: translate("Playing from")
            Text(heading, style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(it, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun PlayerActions(onOutput: () -> Unit, onPreferences: () -> Unit, onQueue: () -> Unit,
    onLyrics: () -> Unit, onVisualizer: () -> Unit, lyricsEnabled: Boolean, visualizerEnabled: Boolean, combined: Boolean) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        PlayerFooterButton("rufin-speaker-front-symbolic", "Output", onOutput)
        PlayerFooterButton("rufin-mixer-sliders-symbolic", "Preferences", onPreferences)
        Spacer(Modifier.weight(1f))
        if (visualizerEnabled && !combined) PlayerFooterButton("rufin-sound-symbolic", "Visualizer", onVisualizer)
        if (lyricsEnabled) PlayerFooterButton("rufin-text-center-symbolic", "Lyrics", onLyrics)
        PlayerFooterButton("rufin-queue-symbolic", "Queue", onQueue)
    }
}

@Composable
private fun PlayerFooterButton(icon: String, label: String, action: () -> Unit) {
    FilledTonalIconButton(action, Modifier.padding(horizontal = 2.dp).size(40.dp), shape = RoundedCornerShape(10.dp),
        colors = IconButtonDefaults.filledTonalIconButtonColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            contentColor = MaterialTheme.colorScheme.onSurface)) {
        RufinIcon(icon, translate(label), Modifier.size(20.dp), glyph = true)
    }
}


@OptIn(ExperimentalAnimationApi::class)
@Composable
private fun PlayerCover(player: PlayerConnection, modifier: Modifier) {
    val duration = if (LocalReduceMotion.current) 0 else 240
    val cover = player.displayedArtworkIdentity to player.artwork
    updateTransition(cover, label = "playerArtwork").Crossfade(modifier.aspectRatio(1f).clip(RoundedCornerShape(8.dp)),
        animationSpec = tween(if (duration == 0) 0 else 220), contentKey = { it.first }) { (_, artwork) ->
        Artwork(artwork, Modifier.fillMaxSize())
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PlayerMetadata(player: PlayerConnection, modifier: Modifier, onGoTo: (String) -> Unit) {
    val reduceMotion = LocalReduceMotion.current
    val metadata by remember(player) { derivedStateOf {
        player.playback?.let { state ->
            val links = player.metadataLinks?.takeIf { it.mediaUri == state.mediaUri }
            PlayerMetadataText(state.mediaUri, state.title, links?.artistText ?: state.artist, links?.albumText ?: state.album)
        }
    } }
    val current = metadata ?: return
    AnimatedContent(current, modifier, transitionSpec = {
        (slideInHorizontally(tween(if (reduceMotion) 0 else 200)) { it / 5 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
            (slideOutHorizontally(tween(if (reduceMotion) 0 else 200)) { -it / 5 } + fadeOut(tween(if (reduceMotion) 0 else 100)))
    }, label = "playerMetadata") { displayed ->
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f).clickable(interactionSource = remember { MutableInteractionSource() },
                indication = null, enabled = displayed.uri != null) { displayed.uri?.let(onGoTo) },
                horizontalAlignment = Alignment.Start, verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(displayed.title, Modifier.fillMaxWidth().rufinMarquee(), style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.Bold, color = MaterialTheme.colorScheme.onSurface, maxLines = 1)
                val subtitle = listOf(displayed.artist, displayed.album).filter(String::isNotBlank).joinToString(" · ")
                if (subtitle.isNotBlank()) Text(subtitle, Modifier.fillMaxWidth().rufinMarquee(), maxLines = 1,
                    style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            val favorite by remember(player) { derivedStateOf {
                val state = player.playback
                state?.mediaUri?.let { player.projectedFavorite(it, state.favorite) } == true
            } }
            IconButton(player::favorite, Modifier.size(48.dp)) {
                RufinIcon(if (favorite) "rufin-heart-filled-symbolic" else "rufin-heart-outline-symbolic",
                    translate("Favorite current track"), Modifier.size(24.dp),
                    if (favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface, glyph = true)
            }
        }
    }
}

private data class PlayerMetadataText(val uri: String?, val title: String, val artist: String, val album: String)

@Composable
private fun PlayerControls(player: PlayerConnection, modifier: Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        PlayerSourceInfo(player)
        Spacer(Modifier.height(8.dp))
        Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) { PlayerProgress(player) }
        Spacer(Modifier.height(16.dp))
        PlayerTransport(player)
    }
}

private data class PlayerSourceFacts(val local: Boolean, val output: String?, val format: String?, val bits: UInt?, val sampleRate: UInt?)

@Composable
private fun PlayerSourceInfo(player: PlayerConnection) {
    val facts by remember(player) { derivedStateOf { player.playback?.let {
        PlayerSourceFacts(it.localOutput, it.outputName, it.sourceFormat, it.bitDepth, it.sampleRateHz)
    } } }
    val current = facts ?: return
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterHorizontally)) {
        Text(current.output ?: translate(if (current.local) "Local" else "Audio output"),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        current.format?.let { Text(it.uppercase(java.util.Locale.ROOT),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        current.bits?.takeIf { it > 0U }?.let { Text("$it bit", style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant) }
        current.sampleRate?.takeIf { it > 0U }?.let {
            val rate = java.math.BigDecimal.valueOf(it.toDouble() / 1000.0).stripTrailingZeros().toPlainString()
            Text("$rate kHz", style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun PlayerProgress(player: PlayerConnection) {
    val current = player.playback ?: return
    var seeking by remember(current.occurrenceId, current.mediaRun) { mutableStateOf<Float?>(null) }
    var dragging by remember(current.occurrenceId, current.mediaRun) { mutableStateOf(false) }
    var committed by remember(current.occurrenceId, current.mediaRun) { mutableStateOf<AndroidPlaybackState?>(null) }
    var position by remember(current.occurrenceId, current.mediaRun) { mutableStateOf(current.positionMillis) }
    val playing = current.positionAdvancing
    val interpolate = playing && !LocalReduceMotion.current
    val duration = current.durationMillis.toFloat()
    val translation = LocalTranslationRevision.current
    val positionLabel = remember(translation) { translate("Playback position") }
    LaunchedEffect(player, current.occurrenceId, current.mediaRun, interpolate) {
        if (interpolate) while (true) {
            withFrameNanos { }
            position = player.positionNow()
        }
    }
    LaunchedEffect(current.positionMillis, current.state, current.canSeek, current.error) {
        val target = seeking?.toLong()?.toULong() ?: return@LaunchedEffect
        if (!dragging && (!current.canSeek || current.state == AndroidTransportState.FAILED ||
            (current.positionMillis != committed?.positionMillis && seekPreviewMatchesPosition(target, current.positionMillis)))) {
            position = current.positionMillis
            seeking = null
            committed = null
        }
    }
    val displayedPosition = seeking?.toLong()?.toULong() ?: if (interpolate) position else current.positionMillis
    val waveform = player.waveform?.takeIf { it.occurrenceId == current.occurrenceId }?.peaks.orEmpty()
    val activeWaveform = MaterialTheme.colorScheme.primary
    val inactiveWaveform = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.35f)
    val thumbWidth = if (waveform.isEmpty()) 14.dp else 2.dp
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    Column(Modifier.fillMaxWidth()) {
        Slider(value = displayedPosition.toFloat().coerceIn(0f, duration.coerceAtLeast(1f)),
            onValueChange = { dragging = true; seeking = it }, onValueChangeFinished = {
                committed = current
                dragging = false
                seeking?.let { player.seek(it.toLong().toULong()) }
            }, valueRange = 0f..duration.coerceAtLeast(1f), enabled = current.canSeek && duration > 0f,
            thumb = {
                val color = if (current.canSeek && duration > 0f) MaterialTheme.colorScheme.primary
                    else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f)
                Canvas(Modifier.width(thumbWidth).height(24.dp)) {
                    if (waveform.isEmpty()) drawCircle(color, radius = 7.dp.toPx(), center = center)
                    else drawLine(color, Offset(center.x, 0f), Offset(center.x, size.height),
                        strokeWidth = 2.dp.toPx(), cap = StrokeCap.Round)
                }
            },
            track = { state ->
                Canvas(Modifier.fillMaxWidth().height(24.dp)) {
                    val fraction = (state.value / duration.coerceAtLeast(1f)).coerceIn(0f, 1f)
                    if (waveform.isEmpty()) {
                        val start = Offset(if (rtl) size.width else 0f, center.y)
                        val end = Offset(if (rtl) 0f else size.width, center.y)
                        drawLine(inactiveWaveform, start, end, strokeWidth = 3.5.dp.toPx(), cap = StrokeCap.Round)
                        if (fraction > 0f) drawLine(activeWaveform, start,
                            Offset(size.width * if (rtl) 1f - fraction else fraction, center.y),
                            strokeWidth = 5.5.dp.toPx(), cap = StrokeCap.Round)
                    } else {
                        val count = (size.width / 4.dp.toPx()).toInt().coerceAtLeast(1)
                        for (i in 0 until count) {
                            val amplitude = waveform[(i.toLong() * waveform.size / count).toInt()].toFloat().coerceIn(0f, 1f)
                            val x = (if (rtl) count - i - 0.5f else i + 0.5f) * size.width / count
                            val height = (amplitude * size.height / 2).coerceAtLeast(1.dp.toPx())
                            drawLine(if (i.toDouble() / count < fraction) activeWaveform else inactiveWaveform,
                                Offset(x, size.height / 2 - height), Offset(x, size.height / 2 + height), strokeWidth = 2.dp.toPx(), cap = StrokeCap.Round)
                        }
                    }
                }
            },
            modifier = Modifier.fillMaxWidth().height(24.dp).layout { measurable, constraints ->
                // Extend the slider's thumb inset so the track spans the same width as the timestamps.
                val inset = thumbWidth.roundToPx()
                val placeable = measurable.measure(constraints.offset(horizontal = inset))
                layout(placeable.width - inset, placeable.height) { placeable.placeRelative(-inset / 2, 0) }
            }.semantics { contentDescription = positionLabel })
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(playerTime(displayedPosition), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(playerTime(current.durationMillis), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

private data class PlayerTransportState(val shuffle: Boolean, val repeat: AndroidRepeatMode,
    val desiredPlaying: Boolean, val canPrevious: Boolean, val canNext: Boolean)

@Composable
private fun PlayerTransport(player: PlayerConnection) {
    val transport by remember(player) { derivedStateOf {
        player.playback?.let { PlayerTransportState(it.shuffle, it.repeat, it.desiredPlaying, it.canPrevious, it.canNext) }
    } }
    val current = transport ?: return
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
            PlayerIconButton("rufin-shuffle-symbolic", "Shuffle", player::shuffle,
                Modifier.size(48.dp).semantics { selected = current.shuffle }, selected = current.shuffle, iconSize = 24.dp, glyph = true)
        }
        Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
            PlayerIconButton("rufin-media-skip-backward-symbolic", "Previous Track", player::previous,
                Modifier.size(48.dp), enabled = current.canPrevious, iconSize = 24.dp, glyph = true)
        }
        Box(Modifier.weight(1.25f), contentAlignment = Alignment.Center) {
            PlayerPlayPause(player)
        }
        Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
            PlayerIconButton("rufin-media-skip-forward-symbolic", "Next Track", player::next,
                Modifier.size(48.dp), enabled = current.canNext, iconSize = 24.dp, glyph = true)
        }
        Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
            PlayerIconButton(if (current.repeat == AndroidRepeatMode.ONE) "rufin-repeat-one-symbolic" else "rufin-repeat-symbolic",
                when (current.repeat) { AndroidRepeatMode.ONE -> "Repeat one"; AndroidRepeatMode.ALL -> "Repeat all"; else -> "Repeat off" },
                player::repeat, Modifier.size(48.dp), selected = current.repeat != AndroidRepeatMode.OFF, iconSize = 24.dp, glyph = true)
        }
    }
}

@Composable
private fun PlayerPlayPause(player: PlayerConnection) {
    val playing by remember(player) { derivedStateOf { player.playback?.desiredPlaying == true } }
    FilledIconButton(player::togglePlayback, Modifier.size(64.dp), shape = CircleShape,
        colors = IconButtonDefaults.filledIconButtonColors(containerColor = MaterialTheme.colorScheme.onBackground.copy(alpha = .92f),
            contentColor = MaterialTheme.colorScheme.background)) {
        Crossfade(playing, animationSpec = tween(if (LocalReduceMotion.current) 0 else 140), label = "fullPlayerTransport") { active ->
            RufinIcon(if (active) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                translate(if (active) "Pause" else "Play"), Modifier.size(28.dp), glyph = true)
        }
    }
}

private fun playerTime(millis: ULong): String {
    val seconds = millis / 1000UL
    return if (seconds >= 3600UL) "${seconds / 3600UL}:${((seconds / 60UL) % 60UL).toString().padStart(2, '0')}:${(seconds % 60UL).toString().padStart(2, '0')}"
    else "${seconds / 60UL}:${(seconds % 60UL).toString().padStart(2, '0')}"
}
