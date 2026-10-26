package io.github.screwys.rufin

import android.content.Context
import android.content.ContextWrapper
import android.net.Uri
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.screwys.rufin.core.AndroidPlaybackUpdate
import io.github.screwys.rufin.core.AndroidRuntime
import io.github.screwys.rufin.shared.library.QueueRepeatMode
import io.github.screwys.rufin.shared.playback.PlaybackNotice
import io.github.screwys.rufin.shared.playback.PlaybackOutput
import io.github.screwys.rufin.shared.playback.TransportStatus
import java.io.File
import java.util.UUID
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
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidAudioRuntimeTest {
    @Test
    fun nativeOutputPlaysEverySampleAfterStartSeekResumeAndRepeat() = runBlocking<Unit> {
        val app = InstrumentationRegistry.getInstrumentation().targetContext
        val root = File(app.cacheDir, "audio-runtime-${UUID.randomUUID()}")
        val files = File(root, "files").apply { check(mkdirs()) }
        val cache = File(root, "cache").apply { check(mkdirs()) }
        val noBackup = File(root, "no-backup").apply { check(mkdirs()) }
        val config = File(files, "config").apply { check(mkdirs()) }
        File(config, "settings.json").writeText(
            """{"playback":{"volume":0.0},"external_metadata_enabled":false}"""
        )
        val media = File(root, "tone.flac")
        InstrumentationRegistry.getInstrumentation().context.assets.open("tone.flac").use { input ->
            media.outputStream().use { input.copyTo(it) }
        }
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
                val transport = runtime.playbackOwner()
                val playback = runtime.subscribePlayback()
                val states = MutableStateFlow<AndroidPlaybackUpdate?>(null)
                val reader = launch(Dispatchers.Default) {
                    while (isActive) {
                        val state = playback.next()
                        val failures = state.notices.filterIsInstance<PlaybackNotice.OperationFailed>()
                        check(state.transport.error == null && failures.isEmpty()) {
                            "Native playback failed: ${state.transport.error}, $failures"
                        }
                        states.value = state
                    }
                }
                try {
                    val clock = Regex("clock_time_millis=(?:Some\\()?([0-9]+)")
                    // Each start opens a new AudioTrack, and a seek refills it.
                    for (phase in listOf("start", "seek", "resume", "restart", "restart", "repeat")) {
                        when (phase) {
                            "start" -> {
                                runtime.openUris(listOf(Uri.fromFile(media).toString()))
                                val started = withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.state == TransportStatus.PLAYING }
                                }
                                assertEquals(PlaybackOutput.Local, started.controls.playbackOutput)
                            }
                            "seek" -> {
                                transport.seekMillis(1_500UL)
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.positionMillis in 1_500UL..2_000UL }
                                }
                            }
                            "resume" -> {
                                transport.pause()
                                val paused = withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.state == TransportStatus.PAUSED }
                                }
                                delay(250)
                                assertEquals(paused.transport.positionMillis, checkNotNull(states.value).transport.positionMillis)
                                transport.play()
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.state == TransportStatus.PLAYING }
                                }
                            }
                            "restart" -> {
                                transport.stop()
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.state == TransportStatus.STOPPED }
                                }
                                transport.play()
                                withTimeout(15_000) {
                                    states.filterNotNull().first { it.transport.state == TransportStatus.PLAYING }
                                }
                            }
                            "repeat" -> {
                                transport.setRepeat(QueueRepeatMode.ONE)
                                transport.seekMillis(5_400UL)
                                withTimeout(15_000) {
                                    states.filterNotNull().first {
                                        it.controls.repeatMode == QueueRepeatMode.ONE && it.transport.positionMillis >= 5_400UL
                                    }
                                }
                                withTimeout(15_000) {
                                    states.filterNotNull().first {
                                        it.transport.state == TransportStatus.PLAYING && it.transport.positionMillis in 200UL..800UL
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
                        // The sink drops audio the device already played past and resyncs.
                        val resync = runtime.diagnosticLog().lineSequence()
                            .firstOrNull { "Unexpected discontinuity" in it }
                        assertEquals("Audio dropped after $phase", null, resync)
                    }
                } finally {
                    transport.stop()
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
