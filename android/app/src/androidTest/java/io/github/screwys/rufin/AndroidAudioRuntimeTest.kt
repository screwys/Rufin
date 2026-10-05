package io.github.screwys.rufin

import android.content.Context
import android.content.ContextWrapper
import android.net.Uri
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidRuntime
import io.github.screwys.rufin.core.AndroidTransportState
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID
import kotlin.math.sin
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidAudioRuntimeTest {
    @Test
    fun nativeOutputAdvancesAfterSeekPauseAndRepeat() = runBlocking<Unit> {
        val app = InstrumentationRegistry.getInstrumentation().targetContext
        val root = File(app.cacheDir, "audio-runtime-${UUID.randomUUID()}")
        val files = File(root, "files").apply { check(mkdirs()) }
        val cache = File(root, "cache").apply { check(mkdirs()) }
        val noBackup = File(root, "no-backup").apply { check(mkdirs()) }
        val config = File(files, "config").apply { check(mkdirs()) }
        File(config, "settings.json").writeText("""
            {"playback":{"audio_output":"rufinandroidaudiosink","volume":0.0},
             "external_metadata_enabled":false,"connect":{"enabled":false},
             "secret_scope_id":"audio-runtime-${UUID.randomUUID()}"}
        """.trimIndent())
        val rate = 48_000
        val frames = rate * 6
        val dataBytes = frames * 4
        val wave = ByteBuffer.allocate(44 + dataBytes).order(ByteOrder.LITTLE_ENDIAN)
        wave.put("RIFF".toByteArray()).putInt(36 + dataBytes).put("WAVEfmt ".toByteArray())
            .putInt(16).putShort(1).putShort(2).putInt(rate).putInt(rate * 4)
            .putShort(4).putShort(16).put("data".toByteArray()).putInt(dataBytes)
        repeat(frames) { frame ->
            val sample = (sin(frame * 2.0 * Math.PI * 440.0 / rate) * 1_000).toInt().toShort()
            wave.putShort(sample).putShort(sample)
        }
        val media = File(root, "generated.wav").apply { writeBytes(wave.array()) }
        val context = object : ContextWrapper(app) {
            override fun getApplicationContext(): Context = this
            override fun getFilesDir(): File = files
            override fun getCacheDir(): File = cache
            override fun getNoBackupFilesDir(): File = noBackup
        }
        try {
            NativeHost.start(context)
            val runtime = AndroidRuntime(files.absolutePath, cache.absolutePath)
            try {
                runtime.setDebugLogging(true)
                val playback = runtime.subscribePlayback()
                val states = MutableStateFlow<AndroidPlaybackState?>(null)
                val reader = launch(Dispatchers.Default) {
                    while (isActive) {
                        val state = playback.next()
                        check(state.error == null && state.operationFailures.isEmpty()) {
                            "Native playback failed: ${state.error}, ${state.operationFailures}"
                        }
                        states.value = state
                    }
                }
                try {
                    val clock = Regex("clock_time_millis=(?:Some\\()?([0-9]+)")
                    for (phase in listOf("start", "seek", "resume", "repeat")) {
                        when (phase) {
                            "start" -> {
                                runtime.openUris(listOf(Uri.fromFile(media).toString()))
                                val started = withTimeout(15_000) {
                                    states.filterNotNull().first { it.state == AndroidTransportState.PLAYING }
                                }
                                assertTrue(started.localOutput)
                            }
                            "seek" -> {
                                runtime.seekMillis(1_500UL)
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.positionMillis in 1_500UL..2_000UL }
                                }
                            }
                            "resume" -> {
                                runtime.pause()
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.state == AndroidTransportState.PAUSED }
                                }
                                delay(250)
                                assertTrue(!checkNotNull(states.value).positionAdvancing)
                                runtime.play()
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.state == AndroidTransportState.PLAYING }
                                }
                            }
                            "repeat" -> {
                                runtime.setRepeat(AndroidRepeatMode.ONE)
                                runtime.seekMillis(5_400UL)
                                withTimeout(15_000) {
                                    states.filterNotNull().first {
                                        it.repeat == AndroidRepeatMode.ONE && it.positionMillis >= 5_400UL
                                    }
                                }
                                withTimeout(15_000) {
                                    states.filterNotNull().first {
                                        it.state == AndroidTransportState.PLAYING && it.positionMillis in 200UL..800UL
                                    }
                                }
                            }
                        }
                        val before = runtime.diagnosticLog().lineSequence()
                            .count { "GStreamer output timing" in it }
                        withTimeout(15_000) {
                            while (true) {
                                val samples = runtime.diagnosticLog().lineSequence()
                                    .filter { "GStreamer output timing" in it }
                                    .drop(before)
                                    .mapNotNull { clock.find(it)?.groupValues?.get(1)?.toLong() }
                                    .toList()
                                if (samples.size >= 3 && samples.last() - samples.first() >= 800) break
                                delay(100)
                            }
                        }
                        val selectedClock = runtime.diagnosticLog().lineSequence()
                            .lastOrNull { "GStreamer playback clock selected" in it }
                        assertTrue("Device clock selected after $phase",
                            selectedClock?.contains("GstAudioSinkClock") == true)
                        assertTrue("Native clock advanced after $phase", checkNotNull(states.value).error == null)
                    }
                } finally {
                    runtime.stop()
                    reader.cancelAndJoin()
                    playback.destroy()
                }
            } finally {
                runtime.destroy()
            }
        } finally {
            root.deleteRecursively()
        }
    }
}
