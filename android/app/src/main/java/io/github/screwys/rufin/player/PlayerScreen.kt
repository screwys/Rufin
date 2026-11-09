package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.clockText
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.Cover
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.MediaNavigationSheet
import io.github.screwys.rufin.settings.LocalTranslationRevision
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.PreferencesConnection

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
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
import io.github.screwys.rufin.ui.CoverShape
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
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.offset
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.text.style.TextOverflow
import io.github.screwys.rufin.shared.library.QueueRepeatMode
import io.github.screwys.rufin.shared.playback.TransportStatus
import io.github.screwys.rufin.core.AndroidPlaybackUpdate
import io.github.screwys.rufin.core.seekPreviewMatchesPosition
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.core.trackCountText
import androidx.compose.animation.Crossfade
import androidx.compose.animation.ExperimentalAnimationApi
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
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
import androidx.compose.ui.draw.drawBehind
import kotlinx.coroutines.launch
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection

private enum class PlayerSurface { LYRICS, VISUALIZER }

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
internal fun PlayerScreen(player: PlayerConnection, preferences: PreferencesConnection, onNavigate: (io.github.screwys.rufin.shared.rufin_core.Route, String, String?) -> Unit, onMore: () -> Unit,
    onSettings: (io.github.screwys.rufin.more.MoreDestination) -> Unit, tiny: Boolean, onClose: () -> Unit) {
    val reduceMotion = LocalReduceMotion.current
    val scope = rememberCoroutineScope()
    var surface by remember { mutableStateOf<PlayerSurface?>(null) }
    var output by remember { mutableStateOf(false) }
    var queue by remember { mutableStateOf(false) }
    var settings by remember { mutableStateOf(false) }
    // Sharing: the card the share sheet shows (null while it is closed), the lines its lyrics card shows
    // (null for those around the playing line), and the lines picked while their selection is open.
    var sharing by remember { mutableStateOf<ShareKind?>(null) }
    var shareLines by remember { mutableStateOf<List<Int>?>(null) }
    var lyricsSelection by remember { mutableStateOf<List<Int>?>(null) }
    val shareStyle = remember { ShareCardStyle() }
    // Picking lines ends back in the share sheet, with the picked lines when they were kept.
    fun finishSelection(keep: Boolean) {
        if (keep) shareLines = lyricsSelection
        lyricsSelection = null
        sharing = ShareKind.LYRICS
    }
    var navigation by remember { mutableStateOf<String?>(null) }
    val lyricsScroll = rememberLazyListState()
    val application = player.settings
    var artworkPixels by remember { mutableIntStateOf(0) }
    // Flip covers have no room for the cover: it becomes the player's blurred background.
    val artwork = rememberArtwork(player.currentItem?.artworkBinding, player, if (tiny) 256 else artworkPixels)
    val lyricsEnabled = application?.display?.fullscreenLyricsVisible != false
    val currentLineEnabled = application?.display?.fullscreenCurrentLyricsLineVisible != false
    val visualizerEnabled = application?.display?.fullscreenVisualizerVisible == true
    val combined = lyricsEnabled && visualizerEnabled && application.rightPanel.combined
    // With the dynamic background on, the player takes its cover's colours (the band's dark stops),
    // white content, boxes a shade darker than the background (as Navic's) and a pastel of the
    // cover for Play; off, the theme's, its boxes in the secondary container.
    val base = MaterialTheme.colorScheme
    val stops = artwork.image?.band?.takeIf { application?.display?.fullscreenDynamicBackground != false }
    val colourTime = tween<Color>(if (reduceMotion) 0 else 320)
    val top by animateColorAsState(stops?.get(0) ?: base.surface, colourTime, label = "playerTop")
    val bottom by animateColorAsState(stops?.get(2) ?: base.surface, colourTime, label = "playerBottom")
    val accent by animateColorAsState(stops?.let { lerp(it[0], Color.White, .62f) } ?: base.primary, colourTime, label = "playerAccent")
    val onAccent by animateColorAsState(stops?.get(2) ?: base.onPrimary, colourTime, label = "playerOnAccent")
    val scheme = if (stops == null) base.copy(surfaceContainerHigh = base.secondaryContainer, primary = accent, onPrimary = onAccent)
        // Its menus and sheets take a darker shade of the cover too, so their white text reads.
        else base.copy(surface = stops[1], onSurface = io.github.screwys.rufin.browse.OnBand,
            background = stops[1], surfaceContainerLowest = stops[2], surfaceContainerLow = lerp(stops[1], Color.Black, .3f),
            surfaceContainer = lerp(stops[1], Color.Black, .22f),
            onSurfaceVariant = io.github.screwys.rufin.browse.OnBand.copy(alpha = .72f),
            surfaceContainerHigh = PlayerShade, surfaceContainerHighest = PlayerShade,
            secondaryContainer = PlayerShade, onSecondaryContainer = io.github.screwys.rufin.browse.OnBand,
            outlineVariant = Color.White.copy(alpha = .28f), primary = accent, onPrimary = onAccent)
    // The covers either side follow the finger while a swipe changes the track.
    val currentIndex by remember(player) { derivedStateOf { player.playback?.queue?.currentIndex } }
    val queueRevision by remember(player) { derivedStateOf { player.playback?.queue?.revision } }
    var neighbours by remember { mutableStateOf<Pair<ByteArray?, ByteArray?>>(null to null) }
    LaunchedEffect(currentIndex, queueRevision) {
        val index = currentIndex ?: return@LaunchedEffect
        neighbours = runCatching {
            val before = if (index > 0UL) player.queuePage(index - 1UL)?.rows?.firstOrNull()?.track()?.artworkBinding else null
            val after = player.queuePage(index + 1UL)?.rows?.firstOrNull()?.track()?.artworkBinding
            before to after
        }.getOrDefault(null to null)
    }
    val verticalOffset = remember { Animatable(0f) }
    val threshold = with(LocalDensity.current) { 48.dp.toPx() }
    // A pull down closes the lyrics or the visualizer back to the player as it closes the player.
    val surfaceOffset = remember { Animatable(0f) }
    fun releaseSurface(distance: Float) {
        scope.launch {
            if (distance > threshold) {
                surface = null
                // The view keeps its place while it fades out.
                kotlinx.coroutines.delay(if (reduceMotion) 0L else 200L)
                surfaceOffset.snapTo(0f)
            } else surfaceOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180))
        }
    }
    // Inside the lyrics a pull past their top does the same.
    val surfacePull = remember(scope) { object : NestedScrollConnection {
        override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
            val current = surfaceOffset.value
            if (current <= 0f || available.y >= 0f || source != NestedScrollSource.UserInput) return Offset.Zero
            val next = (current + available.y).coerceAtLeast(0f)
            scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { surfaceOffset.snapTo(next) }
            return Offset(0f, next - current)
        }
        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            if (available.y <= 0f || source != NestedScrollSource.UserInput) return Offset.Zero
            scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { surfaceOffset.snapTo(surfaceOffset.value + available.y) }
            return Offset(0f, available.y)
        }
        override suspend fun onPreFling(available: Velocity): Velocity {
            if (surfaceOffset.value <= 0f) return Velocity.Zero
            releaseSurface(surfaceOffset.value)
            return available
        }
    } }
    val trackOffset = remember { Animatable(0f) }
    // Picking lines to share shows the lyrics even where the player hides them.
    LaunchedEffect(lyricsEnabled, visualizerEnabled) {
        if (surface == PlayerSurface.LYRICS && !lyricsEnabled && lyricsSelection == null || surface == PlayerSurface.VISUALIZER && !visualizerEnabled) surface = null
    }
    LaunchedEffect(surface, combined, player.playback?.controls?.playbackOutput) { player.visualizer(surface == PlayerSurface.VISUALIZER || surface == PlayerSurface.LYRICS && combined) }
    DisposableEffect(player) { onDispose { player.visualizer(false) } }
    LaunchedEffect(player.currentId) { shareLines = null; lyricsSelection = null }
    LaunchedEffect(surface) { if (surface != PlayerSurface.LYRICS) lyricsSelection = null }
    BackHandler { if (lyricsSelection != null) finishSelection(false) else if (surface != null) surface = null else onClose() }
    MaterialTheme(colorScheme = scheme) {
    Surface(Modifier.fillMaxSize().graphicsLayer { translationY = verticalOffset.value }
        .drawBehind { drawRect(Brush.verticalGradient(listOf(top, bottom))) }
        .pointerInput(surface, threshold, reduceMotion) {
            val open = surface != null
            val offset = if (open) surfaceOffset else verticalOffset
            var distance = 0f
            detectVerticalDragGestures(onDragStart = { distance = 0f }, onVerticalDrag = { change, amount ->
                distance = (distance + amount).coerceAtLeast(0f)
                change.consume()
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { offset.snapTo(distance) }
            }, onDragEnd = {
                if (open) releaseSurface(distance)
                else if (distance > threshold) onClose()
                else scope.launch { verticalOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) }
            }, onDragCancel = { scope.launch { offset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) } })
        }.pointerInput(surface, threshold, reduceMotion, lyricsEnabled, visualizerEnabled) {
            // Over the player a swipe changes the track; over the lyrics and the visualizer it moves between them.
            val shown = surface
            var distance = 0f
            detectHorizontalDragGestures(onDragStart = {
                distance = 0f
                scope.launch { trackOffset.stop() }
            }, onHorizontalDrag = { change, amount ->
                change.consume()
                distance += amount
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { trackOffset.snapTo(distance) }
            }, onDragEnd = {
                when (shown) {
                    null -> if (distance < -threshold && player.playback?.queue?.canNext == true) player.next()
                        else if (distance > threshold && player.currentId != null) player.previous()
                    PlayerSurface.LYRICS -> if (distance < -threshold && visualizerEnabled && lyricsSelection == null)
                        surface = PlayerSurface.VISUALIZER
                    PlayerSurface.VISUALIZER -> if (distance > threshold && lyricsEnabled) surface = PlayerSurface.LYRICS
                }
                scope.launch { trackOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) }
            }, onDragCancel = { scope.launch { trackOffset.animateTo(0f, tween(if (reduceMotion) 0 else 180)) } })
        }, color = Color.Transparent, contentColor = MaterialTheme.colorScheme.onSurface) {
        Box(Modifier.fillMaxSize()) {
            if (tiny || application?.display?.fullscreenBackgroundImage == true) artwork.image?.let { artwork ->
                Image(artwork.bitmap.asImageBitmap(), null, Modifier.matchParentSize().blur(48.dp).alpha(if (tiny) .35f else .12f), contentScale = ContentScale.Crop)
            }
            AnimatedContent(surface, Modifier.fillMaxSize(), transitionSpec = {
                val towardsVisualizer = targetState == PlayerSurface.VISUALIZER
                // Between the lyrics and the visualizer the views slide sideways, as the swipe went.
                if (initialState != null && targetState != null) (slideInHorizontally(tween(if (reduceMotion) 0 else 260)) { if (towardsVisualizer) it / 3 else -it / 3 } +
                    fadeIn(tween(if (reduceMotion) 0 else 200))) togetherWith (slideOutHorizontally(tween(if (reduceMotion) 0 else 260)) {
                        if (towardsVisualizer) -it / 3 else it / 3 } + fadeOut(tween(if (reduceMotion) 0 else 140)))
                else (if (targetState == null) fadeIn(tween(if (reduceMotion) 0 else 180))
                else slideInVertically(tween(if (reduceMotion) 0 else 240)) { it / 8 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
                    fadeOut(tween(if (reduceMotion) 0 else 120))
            }, label = "playerSurface") { visible ->
                Column(Modifier.fillMaxSize().then(if (visible == null) Modifier else Modifier.nestedScroll(surfacePull).graphicsLayer {
                    translationY = surfaceOffset.value
                    alpha = 1f - (surfaceOffset.value / size.height * 1.5f).coerceIn(0f, .6f)
                }).safeDrawingPadding()) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                        val picked = lyricsSelection.takeIf { visible == PlayerSurface.LYRICS }
                        if (picked != null) PlayerIconButton("rufin-window-close-symbolic", "Cancel", { finishSelection(false) })
                        else PlayerIconButton("rufin-go-down-symbolic", "Close", { if (visible == null) onClose() else surface = null })
                        if (picked != null) Text(translate("{count} selected").replace("{count}", picked.size.toString()),
                            Modifier.weight(1f), textAlign = TextAlign.Center, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                        else if (visible == null) PlayerContext(player, Modifier.weight(1f).padding(horizontal = 8.dp))
                        else Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                            Text(player.currentItem?.metadata?.title.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                            Text(player.currentItem?.metadata?.artist.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                        }
                        // The track's menu: its details, favorite, playlists and the rest.
                        if (visible == null) PlayerIconButton("rufin-view-more-symbolic", "More actions", onMore)
                        else if (picked != null) PlayerIconButton("rufin-object-select-symbolic", "Done", { finishSelection(true) }, enabled = picked.isNotEmpty())
                        else if (visible == PlayerSurface.LYRICS) LyricsActions(player)
                        else Spacer(Modifier.size(48.dp))
                    }
                    if (visible == null) {
                        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
                            val viewportHeight = maxHeight
                            val content: @Composable (Modifier) -> Unit = { MainPlayerArtwork(player, artwork, currentLineEnabled, it.clipToBounds(),
                                trackOffset.value, neighbours) { artworkPixels = it } }
                            val controls: @Composable () -> Unit = {
                                PlayerMetadata(player, Modifier.fillMaxWidth().padding(vertical = 12.dp)) { navigation = it }
                                PlayerControls(player, Modifier.fillMaxWidth())
                            }
                            if (tiny) Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)
                                .graphicsLayer { translationX = trackOffset.value }, verticalArrangement = Arrangement.Center,
                                horizontalAlignment = Alignment.CenterHorizontally) {
                                // Title, seekbar and transport only; the format chips would push the transport off screen.
                                PlayerMetadata(player, Modifier.fillMaxWidth()) { navigation = it }
                                Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) { PlayerProgress(player) }
                                PlayerTransport(player)
                            }
                            // Wide panes keep the lyrics, or the visualizer, beside the player.
                            else if (maxWidth >= 600.dp && maxWidth <= maxHeight * 1.6f && (lyricsEnabled || visualizerEnabled)) Row(Modifier.fillMaxSize().padding(horizontal = 16.dp)) {
                                Column(Modifier.weight(1f).fillMaxHeight(), horizontalAlignment = Alignment.CenterHorizontally) {
                                    content(Modifier.weight(1f).fillMaxWidth())
                                    controls()
                                }
                                Spacer(Modifier.width(24.dp))
                                Box(Modifier.weight(1f).fillMaxHeight().padding(vertical = 8.dp)) {
                                    if (visualizerEnabled && (!lyricsEnabled || combined)) VisualizerView(player, Modifier.matchParentSize(),
                                        if (combined) player.settings?.visualizer?.appearance?.fullscreenLyricsOpacity?.toFloat() ?: 1f else 1f)
                                    if (lyricsEnabled) LyricsPanel(player, lyricsScroll)
                                }
                            // Cover beside the controls only when each half has room for the transport.
                            } else if (maxWidth > maxHeight && maxWidth >= 600.dp) Row(Modifier.fillMaxSize().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                                content(Modifier.weight(1f).fillMaxHeight())
                                Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), horizontalAlignment = Alignment.CenterHorizontally) { controls() }
                            } else if (maxHeight >= 360.dp * LocalDensity.current.fontScale.coerceAtLeast(1f)) {
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
                        PlayerActions(player, { output = true }, { sharing = ShareKind.SONG },
                            // Lyrics, or the visualizer while the lyrics are hidden.
                            if (lyricsEnabled) PlayerSurface.LYRICS else PlayerSurface.VISUALIZER.takeIf { visualizerEnabled },
                            { surface = it }, { settings = true }, { queue = true })
                    } else {
                        Box(Modifier.weight(1f).fillMaxWidth().padding(top = 8.dp, bottom = 20.dp)
                            .clipToBounds().graphicsLayer { translationX = trackOffset.value }) {
                            if (visible == PlayerSurface.VISUALIZER || combined) {
                                val opacity = player.settings?.visualizer?.appearance?.fullscreenLyricsOpacity?.toFloat() ?: 1f
                                VisualizerView(player, Modifier.matchParentSize(), if (combined) opacity else 1f)
                            }
                            if (visible == PlayerSurface.LYRICS) LyricsPanel(player, lyricsScroll, lyricsSelection) { lyricsSelection = it }
                        }
                        Box(Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(top = 8.dp)) { PlayerProgress(player) }
                        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(top = 16.dp, bottom = 24.dp), verticalAlignment = Alignment.CenterVertically) {
                            PlayerFooterButton("rufin-speaker-front-symbolic", "Output", { output = true })
                            Spacer(Modifier.weight(1f))
                            PlayerIconButton("rufin-media-skip-backward-symbolic", "Previous Track", player::previous,
                                Modifier.size(48.dp), enabled = player.currentId != null, iconSize = 24.dp, glyph = true)
                            Spacer(Modifier.width(12.dp))
                            PlayerPlayPause(player)
                            Spacer(Modifier.width(12.dp))
                            PlayerIconButton("rufin-media-skip-forward-symbolic", "Next Track", player::next,
                                Modifier.size(48.dp), enabled = player.playback?.queue?.canNext == true, iconSize = 24.dp, glyph = true)
                            Spacer(Modifier.weight(1f))
                            PlayerFooterButton("rufin-queue-symbolic", "Queue", { queue = true })
                        }
                    }
                }
            }
        }
    }
    }
    if (queue) PlayerQueueSheet(player, onDismiss = { queue = false },
        onNavigate = { route, title, source -> queue = false; onNavigate(route, title, source) })
    if (output) PlayerOutputSheet(player) { output = false }
    AnimatedVisibility(settings, enter = slideInHorizontally(tween(if (reduceMotion) 0 else 240)) { it / 4 } + fadeIn(tween(if (reduceMotion) 0 else 180)),
        exit = slideOutHorizontally(tween(if (reduceMotion) 0 else 200)) { it / 4 } + fadeOut(tween(if (reduceMotion) 0 else 140))) {
        PlayerSettingsScreen(player) { settings = false }
    }
    sharing?.let { kind ->
        val item = player.currentItem
        val links = player.metadataLinks?.takeIf { it.mediaUri == player.currentId?.mediaUri }
        ShareSheet(player, ShareSubject(item?.metadata?.title.orEmpty(), item?.metadata?.artist.orEmpty(), item?.artworkBinding,
            "Song", links?.share, lyrics = true), kind, shareStyle, shareLines, onEditLyrics = { lines ->
            sharing = null
            lyricsSelection = lines
            surface = PlayerSurface.LYRICS
        }) { sharing = null }
    }
    navigation?.let { uri -> MediaNavigationSheet(player.metadataLinks?.takeIf { it.mediaUri == uri }, player,
        mediaUri = uri, onNavigate = { route, title, source -> navigation = null; onNavigate(route, title, source) },
        onDismiss = { navigation = null }) }
}

@Composable
private fun MainPlayerArtwork(player: PlayerConnection, artwork: Cover, showCurrentLine: Boolean, modifier: Modifier,
    offset: Float, neighbours: Pair<ByteArray?, ByteArray?>, onArtworkSize: (Int) -> Unit) {
    val reduceMotion = LocalReduceMotion.current
    // The line keeps its place until the new track's lyrics are known, so a track change moves
    // the cover once, not away and back.
    val lyrics = player.currentLyrics
    val known = if (lyrics == null || lyrics.loading) null else lyrics.document?.lines?.isNotEmpty() == true
    var held by remember { mutableStateOf(false) }
    LaunchedEffect(known) { if (known != null) held = known }
    val hasLyrics = showCurrentLine && (known ?: held)
    val space by animateDpAsState(if (hasLyrics) 52.dp else 0.dp, tween(if (reduceMotion) 0 else 260), label = "lyricsPreviewSpace")
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            val side = minOf(maxWidth, maxHeight)
            val step = with(LocalDensity.current) { (side + 24.dp).toPx() }
            val pixels = with(LocalDensity.current) { side.roundToPx() }
            // The previous and next covers sit a cover's width away and slide in with the drag.
            if (offset > 0f) neighbours.first?.let { binding ->
                Artwork(rememberArtwork(binding, player, pixels), Modifier.size(side).graphicsLayer { translationX = offset - step }.clip(CoverShape))
            }
            if (offset < 0f) neighbours.second?.let { binding ->
                Artwork(rememberArtwork(binding, player, pixels), Modifier.size(side).graphicsLayer { translationX = offset + step }.clip(CoverShape))
            }
            PlayerCover(player, artwork, Modifier.size(side).graphicsLayer { translationX = offset }
                .onSizeChanged { onArtworkSize(maxOf(it.width, it.height)) })
        }
        Box(Modifier.fillMaxWidth().height(space), contentAlignment = Alignment.CenterStart) {
            if (showCurrentLine) CurrentLyricsPreview(player)
        }
    }
}

@Composable
private fun PlayerContext(player: PlayerConnection, modifier: Modifier) {
    val context by remember(player) { derivedStateOf { player.currentContext } }
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

/** The output pill on the left; Share, Lyrics (or the visualizer), Settings and Queue on the right. */
@Composable
private fun PlayerActions(player: PlayerConnection, onOutput: () -> Unit, onShare: () -> Unit, view: PlayerSurface?,
    onView: (PlayerSurface) -> Unit, onSettings: () -> Unit, onQueue: () -> Unit) {
    val routes = player.service?.outputs?.routes?.collectAsState()?.value.orEmpty()
    val outputName = routes.firstOrNull { it.selected }?.name ?: translate("Audio output")
    // The pill gives way to the buttons when they leave it no room.
    Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween) {
        Surface(onOutput, Modifier.weight(1f, fill = false).padding(end = 8.dp).height(36.dp).widthIn(max = 220.dp), shape = RoundedCornerShape(18.dp),
            color = MaterialTheme.colorScheme.surfaceContainerHigh) {
            Row(Modifier.padding(start = 12.dp, end = 16.dp), verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RufinIcon("rufin-speaker-front-symbolic", translate("Audio output"), Modifier.size(18.dp), glyph = true)
                Text(outputName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelLarge)
            }
        }
        Row {
            PlayerFooterButton("rufin-share-symbolic", "Share", onShare)
            when (view) {
                PlayerSurface.LYRICS -> PlayerFooterButton("rufin-text-center-symbolic", "Lyrics") { onView(view) }
                PlayerSurface.VISUALIZER -> PlayerFooterButton("rufin-sound-symbolic", "Visualizer") { onView(view) }
                null -> Unit
            }
            PlayerFooterButton("rufin-mixer-sliders-symbolic", "Settings", onSettings)
            PlayerFooterButton("rufin-queue-symbolic", "Queue", onQueue)
        }
    }
}

@Composable
private fun PlayerFooterButton(icon: String, label: String, action: () -> Unit) {
    FilledTonalIconButton(action, Modifier.padding(horizontal = 2.dp).size(36.dp), shape = RoundedCornerShape(10.dp),
        colors = IconButtonDefaults.filledTonalIconButtonColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            contentColor = MaterialTheme.colorScheme.onSurface)) {
        RufinIcon(icon, translate(label), Modifier.size(18.dp), glyph = true)
    }
}

/** The player's boxes on the cover's colours: a shade darker than the background, not a lighter grey. */
private val PlayerShade = Color.Black.copy(alpha = .24f)


@OptIn(ExperimentalAnimationApi::class)
@Composable
private fun PlayerCover(player: PlayerConnection, artwork: Cover, modifier: Modifier) {
    val duration = if (LocalReduceMotion.current) 0 else 240
    val binding = player.currentItem?.artworkBinding?.toList()
    val identity = remember(player.bridge, binding, player.artworkGeneration) {
        binding?.let { player.bridge?.artworkIdentity(it.toByteArray()) }
    }
    val cover = identity to artwork
    updateTransition(cover, label = "playerArtwork").Crossfade(modifier.aspectRatio(1f).clip(CoverShape),
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
            val links = player.metadataLinks?.takeIf { it.mediaUri == state.transport.current?.id?.mediaUri }
            PlayerMetadataText(state.transport.current?.id?.mediaUri, player.currentItem?.metadata?.title.orEmpty(), links?.artistText ?: player.currentItem?.metadata?.artist.orEmpty(), links?.albumText ?: player.currentItem?.metadata?.album.orEmpty())
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
                state?.transport?.current?.id?.mediaUri != null && player.favorite
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

@Composable
private fun PlayerSourceInfo(player: PlayerConnection) {
    val labels by remember(player) { derivedStateOf { player.sourceLabels } }
    FlowRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalArrangement = Arrangement.spacedBy(6.dp)) {
        labels.forEach { label ->
            // Filled like the player's other boxes, so a chip reads as one against any background.
            Surface(shape = CircleShape, color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Text(label, Modifier.padding(horizontal = 10.dp, vertical = 2.dp),
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurface)
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun PlayerProgress(player: PlayerConnection) {
    val current = player.playback ?: return
    var seeking by remember(current.transport.current?.id) { mutableStateOf<Float?>(null) }
    var dragging by remember(current.transport.current?.id) { mutableStateOf(false) }
    var committed by remember(current.transport.current?.id) { mutableStateOf<AndroidPlaybackUpdate?>(null) }
    var position by remember(current.transport.current?.id) { mutableStateOf(current.transport.positionMillis) }
    val playing = (current.transport.state == TransportStatus.PLAYING)
    val interpolate = playing && !LocalReduceMotion.current
    val duration = current.transport.durationMillis.toFloat()
    val translation = LocalTranslationRevision.current
    val positionLabel = remember(translation) { translate("Playback position") }
    LaunchedEffect(player, current.transport.current?.id, interpolate) {
        if (interpolate) while (true) {
            withFrameNanos { }
            position = player.positionNow()
        }
    }
    LaunchedEffect(current.transport.positionMillis, current.transport.state, current.transport.canSeek, current.transport.error) {
        val target = seeking?.toLong()?.toULong() ?: return@LaunchedEffect
        if (!dragging && (!current.transport.canSeek || current.transport.state == TransportStatus.FAILED ||
            (current.transport.positionMillis != committed?.transport?.positionMillis && seekPreviewMatchesPosition(target, current.transport.positionMillis)))) {
            position = current.transport.positionMillis
            seeking = null
            committed = null
        }
    }
    val displayedPosition = seeking?.toLong()?.toULong() ?: if (interpolate) position else current.transport.positionMillis
    val waveform = player.waveform?.takeIf { it.first == current.transport.current?.id }?.second.orEmpty()
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
            }, valueRange = 0f..duration.coerceAtLeast(1f), enabled = current.transport.canSeek && duration > 0f,
            thumb = {
                val color = if (current.transport.canSeek && duration > 0f) MaterialTheme.colorScheme.primary
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
            Text(clockText(displayedPosition), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(clockText(current.transport.durationMillis), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

private data class PlayerTransportState(val shuffle: Boolean, val repeat: QueueRepeatMode,
    val desiredPlaying: Boolean, val canPrevious: Boolean, val canNext: Boolean)

@Composable
private fun PlayerTransport(player: PlayerConnection) {
    val transport by remember(player) { derivedStateOf {
        player.playback?.let { PlayerTransportState(it.controls.shuffleEnabled, it.controls.repeatMode, it.transport.desiredPlaying, it.transport.current != null, it.queue.canNext) }
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
            PlayerIconButton(if (current.repeat == QueueRepeatMode.ONE) "rufin-repeat-one-symbolic" else "rufin-repeat-symbolic",
                when (current.repeat) { QueueRepeatMode.ONE -> "Repeat one"; QueueRepeatMode.ALL -> "Repeat all"; else -> "Repeat off" },
                player::repeat, Modifier.size(48.dp), selected = current.repeat != QueueRepeatMode.OFF, iconSize = 24.dp, glyph = true)
        }
    }
}

@Composable
private fun PlayerPlayPause(player: PlayerConnection) {
    val playing by remember(player) { derivedStateOf { player.playback?.transport?.desiredPlaying == true } }
    // The box is the button's size, so the ring just outside it does not move the layout.
    Box(Modifier.size(64.dp), contentAlignment = Alignment.Center) {
        if (player.refilling) RefillSpinner(72.dp, MaterialTheme.colorScheme.primary)
        // Play takes the accent, at the size of the transport, not oversized.
        FilledIconButton(player::togglePlayback, Modifier.size(64.dp), shape = CircleShape,
            colors = IconButtonDefaults.filledIconButtonColors(containerColor = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary)) {
            Crossfade(playing, animationSpec = tween(if (LocalReduceMotion.current) 0 else 140), label = "fullPlayerTransport") { active ->
                RufinIcon(if (active) "rufin-media-playback-pause-symbolic" else "rufin-media-playback-start-symbolic",
                    translate(if (active) "Pause" else "Play"), Modifier.size(28.dp), glyph = true)
            }
        }
    }
}

// Matches the desktop refill ring: a thin faint track and a round arc.
@Composable
internal fun RefillSpinner(size: Dp, color: Color = LocalContentColor.current) {
    CircularProgressIndicator(Modifier.requiredSize(size).semantics { contentDescription = translate("Loading...") },
        color, 3.dp, color.copy(alpha = .15f), StrokeCap.Round)
}
