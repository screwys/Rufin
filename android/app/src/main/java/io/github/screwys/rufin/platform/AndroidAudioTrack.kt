package io.github.screwys.rufin.platform

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import java.nio.ByteBuffer

internal class AndroidAudioTrack(rate: Int, channels: Int, encoding: Int, bufferBytes: Int) {
    private val accounting = Any()
    private val writing = Any()
    private var epoch = 0L
    private var writtenFrames = 0L
    private var flushing = false
    private var pendingWrite: ByteBuffer? = null
    private var pausedHead: Long? = null
    private val frameBytes = channels * when (encoding) {
        AudioFormat.ENCODING_PCM_16BIT -> 2
        AudioFormat.ENCODING_PCM_FLOAT -> 4
        else -> throw IllegalArgumentException("Unsupported PCM encoding: $encoding")
    }
    private val track: AudioTrack

    init {
        val channelMask = when (channels) {
            1 -> AudioFormat.CHANNEL_OUT_MONO
            2 -> AudioFormat.CHANNEL_OUT_STEREO
            else -> throw IllegalArgumentException("Unsupported channel count: $channels")
        }
        val minimum = AudioTrack.getMinBufferSize(rate, channelMask, encoding)
        check(minimum > 0) { "AudioTrack buffer lookup failed: $minimum" }
        track = AudioTrack.Builder()
            .setAudioAttributes(AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build())
            .setAudioFormat(AudioFormat.Builder()
                .setSampleRate(rate)
                .setChannelMask(channelMask)
                .setEncoding(encoding)
                .build())
            .setBufferSizeInBytes(maxOf(minimum, bufferBytes))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
    }

    fun write(buffer: ByteBuffer, size: Int): Int {
        val writeEpoch = synchronized(accounting) {
            if (flushing) return 0
            epoch
        }
        return synchronized(writing) {
            synchronized(accounting) {
                if (flushing || epoch != writeEpoch) return 0
            }
            // The ring can start before its resume hook drains the old tail.
            if (pendingWrite != null) return@synchronized 0
            val written = track.write(buffer, size, AudioTrack.WRITE_BLOCKING)
            var accepted = written
            if (written >= 0) synchronized(accounting) {
                if (epoch == writeEpoch) {
                    if (written < size && track.playState == AudioTrack.PLAYSTATE_PAUSED) {
                        // GstAudioSink discards a segment's unwritten tail on pause.
                        pendingWrite = ByteBuffer.allocateDirect(size - written).apply {
                            put(buffer)
                            flip()
                        }
                        accepted = size
                    }
                    writtenFrames += accepted / frameBytes
                }
            }
            accepted
        }
    }

    fun delay(): Int = synchronized(accounting) {
        val played = pausedHead ?: (track.playbackHeadPosition.toLong() and 0xffff_ffffL)
        val queued = (writtenFrames - played) and 0xffff_ffffL
        // The device can consume a pending write before its byte count returns.
        if (queued <= Int.MAX_VALUE) queued.toInt() else 0
    }

    fun pause() = synchronized(accounting) {
        val head = track.playbackHeadPosition.toLong() and 0xffff_ffffL
        track.pause()
        // The mixer can advance briefly after pause. Keep GStreamer's paused clock still.
        pausedHead = head
    }

    fun resume() = synchronized(writing) {
        synchronized(accounting) {
            track.play()
            pausedHead = null
        }
        // GStreamer's resume hook keeps the next segment behind this write.
        pendingWrite?.let { buffer ->
            while (buffer.hasRemaining()) {
                val written = track.write(buffer, buffer.remaining(), AudioTrack.WRITE_BLOCKING)
                check(written > 0) { "AudioTrack resume write failed: $written" }
            }
        }
        pendingWrite = null
    }

    fun stop() = discard(true)

    fun clear() = discard(false)

    private fun discard(stop: Boolean) {
        val resume = synchronized(accounting) {
            flushing = true
            epoch++
            val playing = track.playState == AudioTrack.PLAYSTATE_PLAYING
            track.pause()
            playing && !stop
        }
        synchronized(writing) {
            synchronized(accounting) {
                track.flush()
                writtenFrames = 0
                pendingWrite = null
                pausedHead = if (resume) null else 0L
                if (stop) track.stop() else if (resume) track.play()
                flushing = false
            }
        }
    }

    fun close() = synchronized(accounting) {
        epoch++
        track.release()
        writtenFrames = 0
        pendingWrite = null
        pausedHead = null
    }
}
