package io.github.screwys.rufin.browse

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlin.math.abs

internal enum class BrowseSwipeAction(val icon: String) {
    Favorite("rufin-heart-filled-symbolic"),
    PlayLater("rufin-go-last-symbolic"),
    PlayNext("rufin-mail-forward-symbolic"),
    Download("rufin-download-symbolic"),
    AddToPlaylist("rufin-playlists-symbolic"),
    PlayRadio("rufin-audio-only-symbolic"),
    PlayRadioNext("rufin-audio-only-symbolic"),
    PlayRadioLater("rufin-audio-only-symbolic");

    val label: String get() = when (this) {
        Favorite -> translate("Favorite")
        PlayLater -> translate("Play Later")
        PlayNext -> translate("Play Next")
        Download -> translate("Download")
        AddToPlaylist -> translate("Add to Playlist")
        PlayRadio -> translate("Play radio")
        PlayRadioNext -> translate("Play radio next")
        PlayRadioLater -> translate("Play radio later")
    }

    fun availableFor(item: BrowseItem): Boolean = when (this) {
        Favorite -> item.row.kind in setOf("track", "album", "album_header", "artist")
        PlayLater, PlayNext, AddToPlaylist -> item.row.kind == "track" || item.row.detailRoute != null
        PlayRadio, PlayRadioNext, PlayRadioLater -> item.row.kind in setOf("track", "album", "album_header", "artist", "genre", "playlist")
        Download -> item.row.kind == "track" || item.row.detailRoute != null
    }
}

private class BrowseGestureActions(
    val left: BrowseSwipeAction, val right: BrowseSwipeAction,
    val activate: (BrowseItem, BrowseSwipeAction) -> Unit,
)

private val LocalBrowseGestureActions = staticCompositionLocalOf<BrowseGestureActions?> { null }

@Composable
internal fun BrowseGestureSettings(left: BrowseSwipeAction, right: BrowseSwipeAction, browse: BrowseConnection,
    playlist: (BrowseItem) -> Unit, content: @Composable () -> Unit,
) {
    val actions = remember(left, right, browse, playlist) {
        BrowseGestureActions(left, right) { item, action ->
            when (action) {
                BrowseSwipeAction.Favorite -> browse.favorite(item)
                BrowseSwipeAction.PlayLater -> browse.play(item, "append")
                BrowseSwipeAction.PlayNext -> browse.play(item, "next")
                BrowseSwipeAction.AddToPlaylist -> playlist(item)
                BrowseSwipeAction.PlayRadio -> browse.radio(item, "now")
                BrowseSwipeAction.PlayRadioNext -> browse.radio(item, "next")
                BrowseSwipeAction.PlayRadioLater -> browse.radio(item, "append")
                BrowseSwipeAction.Download -> browse.download(item)
            }
        }
    }
    CompositionLocalProvider(LocalBrowseGestureActions provides actions, content = content)
}

@Composable
internal fun BrowseSwipeActions(
    item: BrowseItem, enabled: Boolean,
    content: @Composable () -> Unit,
) {
    val configured = LocalBrowseGestureActions.current
    val left = (configured?.left ?: BrowseSwipeAction.Favorite).takeIf { it.availableFor(item) }
    val right = (configured?.right ?: BrowseSwipeAction.PlayLater).takeIf { it.availableFor(item) }
    if (!enabled || configured == null || left == null && right == null) {
        content()
        return
    }
    key(item.row.key, item.index) {
        val threshold = with(LocalDensity.current) { 64.dp.toPx() }
        val scope = rememberCoroutineScope()
        val settled = remember { Animatable(0f) }
        var dragging by remember { mutableStateOf(false) }
        var dragDistance by remember { mutableFloatStateOf(0f) }
        var settling by remember { mutableStateOf<Job?>(null) }
        val distance by remember { derivedStateOf { if (dragging) dragDistance else settled.value } }
        val currentItem by rememberUpdatedState(item)
        val actions by rememberUpdatedState(configured)
        fun finish(activate: Boolean) {
            val released = dragDistance
            settling = scope.launch(start = CoroutineStart.UNDISPATCHED) {
                settled.snapTo(released)
                dragging = false
                if (activate) {
                    val action = when { released < -threshold -> left; released > threshold -> right; else -> null }
                    if (action != null) actions?.activate?.invoke(currentItem, action)
                }
                settled.animateTo(0f, spring(dampingRatio = Spring.DampingRatioNoBouncy,
                    stiffness = Spring.StiffnessMediumLow))
            }
        }
        Box(Modifier.fillMaxWidth().clipToBounds().pointerInput(threshold, left, right) {
            detectHorizontalDragGestures(
                onDragStart = {
                    settling?.cancel()
                    dragDistance = settled.value
                    dragging = true
                },
                onHorizontalDrag = { change, amount ->
                    val next = dragDistance + amount
                    dragDistance = when {
                        next < 0f && left == null -> 0f
                        next > 0f && right == null -> 0f
                        else -> next
                    }
                    if (dragDistance != 0f) change.consume()
                },
                onDragEnd = { finish(true) },
                onDragCancel = { finish(false) },
            )
        }) {
            Box(Modifier.matchParentSize().background(MaterialTheme.colorScheme.primaryContainer).clearAndSetSemantics {}) {
                right?.let { action -> RufinIcon(if (action == BrowseSwipeAction.Favorite && item.row.favorite) "rufin-heart-outline-symbolic" else action.icon,
                    action.label,
                    Modifier.align(Alignment.CenterStart).padding(horizontal = 20.dp).size(24.dp).graphicsLayer {
                        alpha = if (distance > 0f) (distance / threshold).coerceIn(0f, 1f) else 0f
                        val scale = .8f + .2f * (abs(distance) / threshold).coerceIn(0f, 1f)
                        scaleX = scale; scaleY = scale
                    }, tint = if (action == BrowseSwipeAction.Favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onPrimaryContainer) }
                left?.let { action -> RufinIcon(if (action == BrowseSwipeAction.Favorite && item.row.favorite) "rufin-heart-outline-symbolic" else action.icon,
                    action.label, Modifier.align(Alignment.CenterEnd).padding(horizontal = 20.dp).size(24.dp).graphicsLayer {
                        alpha = if (distance < 0f) (-distance / threshold).coerceIn(0f, 1f) else 0f
                        val scale = .8f + .2f * (abs(distance) / threshold).coerceIn(0f, 1f)
                        scaleX = scale; scaleY = scale
                    }, tint = if (action == BrowseSwipeAction.Favorite) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onPrimaryContainer) }
            }
            Box(Modifier.fillMaxWidth().graphicsLayer { translationX = distance }
                .background(MaterialTheme.colorScheme.surface)) { content() }
        }
    }
}
