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

@Composable
internal fun rememberArtwork(identityBytes: ByteArray?, player: PlayerConnection, pixels: Int): PlayerArtwork? {
    val binding = remember(identityBytes) { identityBytes?.toList() } ?: return null
    val bridge = player.bridge
    val identity = remember(bridge, binding) { bridge?.artworkIdentity(binding.toByteArray()) } ?: return null
    val revision = player.artworkRevision
    return key(identity) {
        val cached = remember(bridge, revision, pixels) { player.artworkCache.peek(identity, pixels) }
        val preview = remember(bridge, revision, binding) {
            if (cached == null) bridge?.artworkPreview(binding.toByteArray())?.let { decodeArtwork(it, 32) } else null
        }
        val loaded by produceState(cached ?: preview, bridge, revision, pixels, binding) {
            value = cached ?: value ?: preview
            if (bridge != null && pixels > 0) {
                try { value = player.artworkCache.load(bridge, identity, binding.toByteArray(), pixels) ?: value }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { player.reportError(error) }
            }
        }
        loaded
    }
}

internal data class PlayerArtwork(val bitmap: Bitmap, val color: Color)

internal class ArtworkCache {
    private data class Key(val identity: String, val pixels: Int, val revision: ULong)
    private class Load(val mutex: Mutex = Mutex(), var readers: Int = 0)
    private val loads = HashMap<Key, Load>()
    private val sizes = HashMap<String, TreeMap<Int, Key>>()
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

    fun peek(identity: String, pixels: Int): PlayerArtwork? = synchronized(decoded) {
        decoded.get(Key(identity, pixels, revision)) ?: sizes[identity]?.ceilingEntry(pixels)?.value?.let(decoded::get)
    }

    suspend fun load(player: AndroidPlayer, identity: String, binding: ByteArray, pixels: Int): PlayerArtwork? {
        val key = synchronized(decoded) { Key(identity, pixels, revision) }
        peek(identity, pixels)?.let { return it }
        val load = synchronized(loads) { loads.getOrPut(key) { Load() }.also { it.readers++ } }
        try {
            return load.mutex.withLock {
                peek(identity, pixels)?.let { return@withLock it }
                val artwork = withContext(Dispatchers.Default) {
                    val encoded = player.artwork(binding, pixels.toUInt()) ?: return@withContext null
                    decodePlayerArtwork(encoded, pixels)
                } ?: return@withLock null
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
    decodeArtwork(bytes, maxDimension)
}

private fun decodeArtwork(bytes: ByteArray, maxDimension: Int): PlayerArtwork? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    var sampleSize = 1
    while (minOf(bounds.outWidth, bounds.outHeight) / (sampleSize * 2) >= maxDimension) sampleSize *= 2
    val options = BitmapFactory.Options().apply { inSampleSize = sampleSize }
    val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) ?: return null
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
    return PlayerArtwork(bitmap, color)
}

@Composable
internal fun Artwork(artwork: PlayerArtwork?, modifier: Modifier = Modifier, description: String? = null) {
    val opacity = animateFloatAsState(if (artwork == null) 0f else 1f,
        tween(if (LocalReduceMotion.current) 0 else 160), label = "artworkOpacity").value
    Box(modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest), contentAlignment = Alignment.Center) {
        if (artwork == null || opacity < 1f) FallbackCover(Modifier.fillMaxSize())
        if (artwork != null) {
            Image(artwork.bitmap.asImageBitmap(), description, Modifier.fillMaxSize().graphicsLayer { alpha = opacity }, contentScale = ContentScale.Crop)
        }
    }
}
