package io.github.screwys.rufin.ui

import io.github.screwys.rufin.player.PlayerConnection

import android.graphics.Bitmap
import android.util.Log
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
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
import io.github.screwys.rufin.core.AndroidException
import io.github.screwys.rufin.core.AndroidArtwork
import io.github.screwys.rufin.core.decodeArtwork
import java.nio.ByteBuffer
import io.github.screwys.rufin.core.AndroidPlayer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import io.github.screwys.rufin.settings.LocalReduceMotion
import java.util.TreeMap

@Composable
internal fun rememberArtwork(identityBytes: ByteArray?, player: PlayerConnection, pixels: Int): Cover {
    val binding = remember(identityBytes) { identityBytes?.toList() } ?: return Cover.Missing
    val bridge = player.bridge
    val identity = remember(bridge, binding, player.artworkGeneration) { bridge?.artworkIdentity(binding.toByteArray()) }
        ?: return Cover.Pending
    return key(identity) {
        val cached = remember(bridge, pixels) { player.artworkCache.peek(identity, pixels) }
        val loaded by produceState(cached?.let { Cover(it, false) } ?: Cover.Pending, bridge, pixels, binding) {
            if (cached != null) value = Cover(cached, false)
            if (bridge != null && pixels > 0) {
                try {
                    if (value.image == null) withContext(Dispatchers.Default) {
                        bridge.artworkPreview(binding.toByteArray())?.let(::playerArtwork)
                    }?.let { value = Cover(it, false) }
                    val image = player.artworkCache.load(bridge, identity, binding.toByteArray(), pixels)
                    value = Cover(image ?: value.image, image == null)
                }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { value = Cover(value.image, true); Log.d("Rufin", "Could not load artwork") }
            }
        }
        loaded
    }
}

/** A cover as a tile shows it: its image or preview once read, and whether it has none. Until one of
 *  them is known the tile stays plain, as GTK's pending tile does, so the fallback never precedes a cover. */
internal data class Cover(val image: PlayerArtwork?, val missing: Boolean) {
    /** A decoded image, where none means the item has no cover. */
    constructor(image: PlayerArtwork?) : this(image, image == null)
    companion object {
        val Pending = Cover(null, false)
        val Missing = Cover(null, true)
    }
}

/** A decoded cover, its average colour, and the three stops of the showcase band it heads. */
internal data class PlayerArtwork(val bitmap: Bitmap, val color: Color, val band: List<Color>)

internal class ArtworkCache {
    private data class Key(val identity: String, val pixels: Int)
    private class Load(val mutex: Mutex = Mutex(), var readers: Int = 0)
    private val loads = HashMap<Key, Load>()
    private val sizes = HashMap<String, TreeMap<Int, Key>>()
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

    fun peek(identity: String, pixels: Int): PlayerArtwork? = synchronized(decoded) {
        decoded.get(Key(identity, pixels)) ?: sizes[identity]?.ceilingEntry(pixels)?.value?.let(decoded::get)
    }

    suspend fun load(player: AndroidPlayer, identity: String, binding: ByteArray, pixels: Int): PlayerArtwork? {
        val key = Key(identity, pixels)
        peek(identity, pixels)?.let { return it }
        val load = synchronized(loads) { loads.getOrPut(key) { Load() }.also { it.readers++ } }
        try {
            return load.mutex.withLock {
                peek(identity, pixels)?.let { return@withLock it }
                val artwork = withContext(Dispatchers.Default) {
                    player.artwork(binding, pixels.toUInt())?.let(::playerArtwork)
                } ?: return@withLock null
                synchronized(decoded) {
                    sizes.getOrPut(identity) { TreeMap() }[pixels] = key
                    decoded.put(key, artwork)
                    artwork
                }
            }
        } finally {
            synchronized(loads) { if (--load.readers == 0) loads.remove(key) }
        }
    }
}

// Decode with the shared image reader to preserve formats and orientation.
internal suspend fun decodePlayerArtwork(bytes: ByteArray, maxDimension: Int = 2048): PlayerArtwork? = withContext(Dispatchers.Default) {
    try { playerArtwork(decodeArtwork(bytes, maxDimension.toUInt())) } catch (_: AndroidException) { null }
}

internal fun playerArtwork(image: AndroidArtwork): PlayerArtwork {
    val bitmap = Bitmap.createBitmap(image.width.toInt(), image.height.toInt(), Bitmap.Config.ARGB_8888)
    bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(image.rgba))
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
    return PlayerArtwork(bitmap, color, image.band.map { Color(it.red, it.green, it.blue) })
}

internal val CoverShape = RoundedCornerShape(8.dp)

@Composable
internal fun Artwork(cover: Cover, modifier: Modifier = Modifier, description: String? = null) {
    val artwork = cover.image
    val opacity = animateFloatAsState(if (artwork == null) 0f else 1f,
        tween(if (LocalReduceMotion.current) 0 else 160), label = "artworkOpacity").value
    Box(modifier.background(artwork?.color ?: MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        if (artwork == null && cover.missing) FallbackCover(Modifier.fillMaxSize())
        if (artwork != null) {
            Image(artwork.bitmap.asImageBitmap(), description, Modifier.fillMaxSize().graphicsLayer { alpha = opacity }, contentScale = ContentScale.Crop)
        }
    }
}
