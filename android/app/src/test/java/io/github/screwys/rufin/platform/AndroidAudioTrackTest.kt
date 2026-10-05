package io.github.screwys.rufin.platform

import android.media.AudioFormat
import android.media.AudioTrack
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAudioTrack
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 34], manifest = Config.NONE)
class AndroidAudioTrackTest {
    private lateinit var output: AndroidAudioTrack
    private lateinit var device: AudioTrack
    private val capture = ShadowAudioTrack.OnAudioDataWrittenListener { track, _, _ -> device = track }

    @Before
    fun setUp() {
        output = AndroidAudioTrack(48_000, 2, AudioFormat.ENCODING_PCM_16BIT, 128)
        ShadowAudioTrack.addAudioDataListener(capture)
    }

    @After
    fun tearDown() {
        ShadowAudioTrack.removeAudioDataListener(capture)
        output.close()
    }

    @Test
    fun pauseKeepsAudioAndResumeKeepsItsPosition() {
        output.resume()
        assertEquals(64, output.write(ByteBuffer.allocateDirect(64), 64))
        assertEquals(16, device.playbackHeadPosition)

        output.pause()
        assertEquals(AudioTrack.PLAYSTATE_PAUSED, device.playState)
        assertEquals(16, device.playbackHeadPosition)

        output.resume()
        assertEquals(AudioTrack.PLAYSTATE_PLAYING, device.playState)
        assertEquals(16, device.playbackHeadPosition)
        assertEquals(0, output.delay())
    }

    @Test
    fun lateDeviceHeadUpdateKeepsPausedClockStill() {
        output.resume()
        output.write(ByteBuffer.allocateDirect(64), 64)
        val sdk = shadowOf(device)
        // Model queued SDK audio, then a final mixer update after pause returns.
        ReflectionHelpers.setField(sdk, "numBytesReceived", 32)
        assertEquals(8, output.delay())
        output.pause()
        ReflectionHelpers.setField(sdk, "numBytesReceived", 48)
        assertEquals(12, device.playbackHeadPosition)
        assertEquals(8, output.delay())

        output.resume()
        assertEquals(4, output.delay())
    }

    @Test
    fun monoFloatKeepsNegotiatedFormatAndFrameSize() {
        output.close()
        output = AndroidAudioTrack(44_100, 1, AudioFormat.ENCODING_PCM_FLOAT, 128)
        output.resume()
        assertEquals(64, output.write(ByteBuffer.allocateDirect(64), 64))

        assertEquals(AudioFormat.ENCODING_PCM_FLOAT, device.audioFormat)
        assertEquals(1, device.channelCount)
        assertEquals(44_100, device.sampleRate)
        assertEquals(16, device.playbackHeadPosition)
        assertEquals(0, output.delay())
    }

    @Test
    fun clearDiscardsOldFramesAndResumesPlayingOutput() {
        output.resume()
        output.write(ByteBuffer.allocateDirect(64), 64)
        output.clear()

        assertEquals(AudioTrack.PLAYSTATE_PLAYING, device.playState)
        assertEquals(0, device.playbackHeadPosition)
        assertEquals(0, output.delay())

        output.write(ByteBuffer.allocateDirect(16), 16)
        assertEquals(4, device.playbackHeadPosition)
        assertEquals(0, output.delay())
    }

    @Test
    fun clearKeepsPausedOutputPaused() {
        output.resume()
        output.write(ByteBuffer.allocateDirect(64), 64)
        output.pause()
        output.clear()

        assertEquals(AudioTrack.PLAYSTATE_PAUSED, device.playState)
        assertEquals(0, device.playbackHeadPosition)
        assertEquals(0, output.delay())
    }

    @Test
    fun stopDiscardsAudioAndStartsFromZeroOnResume() {
        output.resume()
        output.write(ByteBuffer.allocateDirect(64), 64)
        output.stop()

        assertEquals(AudioTrack.PLAYSTATE_STOPPED, device.playState)
        assertEquals(0, device.playbackHeadPosition)
        assertEquals(0, output.delay())

        output.resume()
        output.write(ByteBuffer.allocateDirect(16), 16)
        assertEquals(4, device.playbackHeadPosition)
        assertEquals(0, output.delay())
    }

    @Test
    fun closeReleasesDeviceAndReturnsSdkWriteFailure() {
        output.resume()
        output.write(ByteBuffer.allocateDirect(64), 64)
        output.close()

        assertEquals(AudioTrack.STATE_UNINITIALIZED, device.state)
        assertEquals(AudioTrack.ERROR_INVALID_OPERATION,
            output.write(ByteBuffer.allocateDirect(16), 16))
    }

    @Test
    fun clearWaitsForWriteAndRejectsPreClearWaitingAudio() {
        output.resume()
        val entered = CountDownLatch(1)
        val finishWrite = CountDownLatch(1)
        val cleared = CountDownLatch(1)
        val writes = AtomicInteger()
        val firstResult = AtomicInteger(-1)
        val waitingResult = AtomicInteger(-1)
        val duringWrite = ShadowAudioTrack.OnAudioDataWrittenListener { _, _, _ ->
            writes.incrementAndGet()
            entered.countDown()
            assertTrue(finishWrite.await(5, TimeUnit.SECONDS))
        }
        ShadowAudioTrack.addAudioDataListener(duringWrite)
        val writer = Thread { firstResult.set(output.write(ByteBuffer.allocateDirect(64), 64)) }
        val waiting = Thread { waitingResult.set(output.write(ByteBuffer.allocateDirect(16), 16)) }
        val reset = Thread { output.clear(); cleared.countDown() }
        try {
            writer.start()
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            waiting.start()
            var deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (waiting.state != Thread.State.BLOCKED && System.nanoTime() < deadline) Thread.yield()
            assertEquals(Thread.State.BLOCKED, waiting.state)
            reset.start()
            deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (device.playState != AudioTrack.PLAYSTATE_PAUSED && System.nanoTime() < deadline) Thread.yield()
            assertEquals(AudioTrack.PLAYSTATE_PAUSED, device.playState)
            assertEquals(1L, cleared.count)
            assertEquals(16, device.playbackHeadPosition)
        } finally {
            finishWrite.countDown()
            writer.join(5_000)
            waiting.join(5_000)
            reset.join(5_000)
            ShadowAudioTrack.removeAudioDataListener(duringWrite)
        }

        assertEquals(64, firstResult.get())
        assertEquals(0, waitingResult.get())
        assertEquals(1, writes.get())
        assertEquals(0L, cleared.count)
        assertEquals(0, device.playbackHeadPosition)
        assertEquals(0, output.delay())
        assertEquals(AudioTrack.PLAYSTATE_PLAYING, device.playState)
    }
}
