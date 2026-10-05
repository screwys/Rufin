package io.github.screwys.rufin.ui

import io.github.screwys.rufin.player.PlayerConnection

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.semantics
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import io.github.screwys.rufin.core.AndroidPlayer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import io.github.screwys.rufin.settings.LocalReduceMotion
import java.util.TreeMap

private class ArtworkScene {
    var pending by mutableIntStateOf(0)
    var laidOut by mutableStateOf(false)
    var revealed by mutableStateOf(false)
}

private val LocalArtworkScene = staticCompositionLocalOf<ArtworkScene?> { null }
private data class ArtworkLoad(val artwork: PlayerArtwork?, val complete: Boolean)

@Composable
internal fun ArtworkReadyContent(key: Any? = Unit, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    val scene = remember(key) { ArtworkScene() }
    if (scene.laidOut && scene.pending == 0) SideEffect { if (scene.pending == 0) scene.revealed = true }
    Box(modifier.onGloballyPositioned {
        scene.laidOut = true
        if (scene.pending == 0) scene.revealed = true
    }.drawWithContent { if (scene.revealed) drawContent() }
        .semantics { if (!scene.revealed) hideFromAccessibility() }
        .pointerInput(scene) {
            awaitPointerEventScope {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    if (!scene.revealed) event.changes.forEach { it.consume() }
                }
            }
        }) {
        CompositionLocalProvider(LocalArtworkScene provides scene, content = content)
    }
}

@Composable
internal fun rememberArtwork(identityBytes: ByteArray?, player: PlayerConnection, pixels: Int, reuseCached: Boolean = true): PlayerArtwork? {
    val identity = remember(identityBytes) { identityBytes?.toList() } ?: return null
    val bridge = player.bridge
    val revision = player.playback?.artworkRevision ?: 0UL
    val scene = LocalArtworkScene.current
    return key(identity, pixels) {
        val cached = remember(bridge, revision) { if (reuseCached) player.artworkCache.peekNearby(identity, pixels) else player.artworkCache.peek(identity, pixels) }
        val loaded by produceState(ArtworkLoad(cached, cached != null || bridge == null), bridge, revision) {
            if (bridge != null) {
                val visible = cached ?: value.artwork
                value = ArtworkLoad(visible, visible != null)
                try { value = ArtworkLoad(player.artworkCache.load(bridge, identity, pixels), true) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { value = ArtworkLoad(value.artwork, true); player.reportError(error) }
            } else value = ArtworkLoad(cached, true)
        }
        DisposableEffect(scene, loaded.complete) {
            if (scene != null && !loaded.complete) {
                scene.pending++
                onDispose { scene.pending-- }
            } else onDispose {}
        }
        loaded.artwork
    }
}

internal data class PlayerArtwork(val bitmap: Bitmap, val color: Color)

internal class ArtworkCache {
    private data class Key(val identity: List<Byte>, val pixels: Int, val revision: ULong)
    private class Load(val mutex: Mutex = Mutex(), var readers: Int = 0)
    private val loads = HashMap<Key, Load>()
    private val sizes = HashMap<List<Byte>, TreeMap<Int, Key>>()
    private var revision = 0UL
    private val decoded = object : LruCache<Key, PlayerArtwork>(32 * 1024 * 1024) {
        override fun sizeOf(key: Key, value: PlayerArtwork) = value.bitmap.allocationByteCount
        override fun entryRemoved(evicted: Boolean, key: Key, oldValue: PlayerArtwork, newValue: PlayerArtwork?) {
            if (newValue != null) return
            synchronized(this) {
                sizes[key.identity]?.let { available ->
                    available.remove(key.pixels)
                    if (available.isEmpty()) sizes.remove(key.identity)
                }
            }
        }
    }

    fun updateRevision(value: ULong) = synchronized(decoded) {
        if (revision != value) {
            revision = value
            decoded.evictAll()
        }
    }

    fun peek(identity: List<Byte>, pixels: Int): PlayerArtwork? = synchronized(decoded) {
        decoded.get(Key(identity, pixels, revision))
    }

    fun peekNearby(identity: List<Byte>, pixels: Int): PlayerArtwork? = synchronized(decoded) {
        decoded.get(Key(identity, pixels, revision)) ?: sizes[identity]?.let { available ->
            val below = available.floorEntry(pixels)
            val above = available.ceilingEntry(pixels)
            val nearest = when {
                below == null -> above
                above == null -> below
                pixels - below.key < above.key - pixels -> below
                else -> above
            }
            nearest?.value?.let(decoded::get)
        }
    }

    suspend fun load(player: AndroidPlayer, identity: List<Byte>, pixels: Int): PlayerArtwork? {
        val key = synchronized(decoded) { Key(identity, pixels, revision) }
        decoded.get(key)?.let { return it }
        val load = synchronized(loads) { loads.getOrPut(key) { Load() }.also { it.readers++ } }
        try {
            return load.mutex.withLock {
                decoded.get(key)?.let { return@withLock it }
                val encoded = player.artwork(identity.toByteArray(), pixels.toUInt()) ?: return@withLock null
                val artwork = decodePlayerArtwork(encoded.bytes, pixels) ?: return@withLock null
                synchronized(decoded) {
                    if (key.revision != revision) null
                    else {
                        sizes.getOrPut(identity) { TreeMap() }[pixels] = key
                        decoded.put(key, artwork)
                        artwork
                    }
                }
            }
        } finally {
            synchronized(loads) { if (--load.readers == 0) loads.remove(key) }
        }
    }
}

// The shared artwork cache supplies the encoded image. Compose only decodes it for display.
internal suspend fun decodePlayerArtwork(bytes: ByteArray, maxDimension: Int = 2048): PlayerArtwork? = withContext(Dispatchers.Default) {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    var sampleSize = 1
    while (maxOf(bounds.outWidth, bounds.outHeight) / sampleSize > maxDimension) sampleSize *= 2
    val options = BitmapFactory.Options().apply { inSampleSize = sampleSize }
    val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) ?: return@withContext null
    val sample = Bitmap.createScaledBitmap(bitmap, 8, 8, true)
    var red = 0L
    var green = 0L
    var blue = 0L
    var count = 0
    for (y in 0 until sample.height) for (x in 0 until sample.width) {
        val pixel = sample.getPixel(x, y)
        if (android.graphics.Color.alpha(pixel) >= 128) {
            red += android.graphics.Color.red(pixel)
            green += android.graphics.Color.green(pixel)
            blue += android.graphics.Color.blue(pixel)
            count++
        }
    }
    if (sample !== bitmap) sample.recycle()
    val color = if (count == 0) Color.Gray else Color(
        (red / count).toInt(), (green / count).toInt(), (blue / count).toInt(),
    )
    PlayerArtwork(bitmap, color)
}

@Composable
internal fun Artwork(artwork: PlayerArtwork?, modifier: Modifier = Modifier, description: String? = null) {
    val opacity = if (LocalArtworkScene.current != null) 1f else animateFloatAsState(if (artwork == null) 0f else 1f,
        tween(if (LocalReduceMotion.current) 0 else 160), label = "artworkOpacity").value
    Box(modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        if (artwork == null || opacity < 1f) FallbackCover(Modifier.fillMaxSize())
        if (artwork != null) {
            Image(artwork.bitmap.asImageBitmap(), description, Modifier.fillMaxSize().graphicsLayer { alpha = opacity }, contentScale = ContentScale.Crop)
        }
    }
}
