package io.github.screwys.rufin.player

import android.media.AudioManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 34], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AndroidAudioFocusTest {
    @Test
    fun transientInterruptionsResumeLocalPlaybackOnce() {
        val context = RuntimeEnvironment.getApplication()
        val audio = shadowOf(context.getSystemService(AudioManager::class.java))
        var playingLocally = true
        var pauses = 0
        var resumes = 0
        val focus = AndroidAudioFocus(context,
            isPlayingLocally = { playingLocally },
            pause = { pauses++; playingLocally = false },
            resume = { resumes++; playingLocally = true },
        )
        assertTrue(focus.acquire())
        val listener = audio.lastAudioFocusRequest.listener

        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_LOSS_TRANSIENT)
        assertEquals(1, pauses)
        assertTrue(focus.resumeOnGain)
        assertFalse(focus.hasFocus)
        assertNull(audio.lastAbandonedAudioFocusListener)

        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK)
        assertEquals(1, pauses)
        assertTrue(focus.resumeOnGain)
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_GAIN)
        assertEquals(1, resumes)
        assertTrue(focus.hasFocus)
        assertFalse(focus.resumeOnGain)

        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_GAIN)
        assertEquals(1, resumes)
        focus.close()
        assertSame(listener, audio.lastAbandonedAudioFocusListener)
    }

    @Test
    fun manualPauseCancelsResumeAfterAnInterruption() {
        val context = RuntimeEnvironment.getApplication()
        val audio = shadowOf(context.getSystemService(AudioManager::class.java))
        var playingLocally = true
        var resumes = 0
        val focus = AndroidAudioFocus(context,
            isPlayingLocally = { playingLocally },
            pause = { playingLocally = false },
            resume = { resumes++; playingLocally = true },
        )
        assertTrue(focus.acquire())
        val listener = audio.lastAudioFocusRequest.listener
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_LOSS_TRANSIENT)
        assertTrue(focus.resumeOnGain)

        focus.abandon()
        assertFalse(focus.resumeOnGain)
        assertSame(listener, audio.lastAbandonedAudioFocusListener)
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_GAIN)
        assertEquals(0, resumes)
        assertFalse(playingLocally)
        focus.close()
    }

    @Test
    fun permanentFocusLossDoesNotResumeOnLaterGain() {
        val context = RuntimeEnvironment.getApplication()
        val audio = shadowOf(context.getSystemService(AudioManager::class.java))
        var playingLocally = true
        var pauses = 0
        var resumes = 0
        val focus = AndroidAudioFocus(context,
            isPlayingLocally = { playingLocally },
            pause = { pauses++; playingLocally = false },
            resume = { resumes++; playingLocally = true },
        )
        assertTrue(focus.acquire())
        val listener = audio.lastAudioFocusRequest.listener
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_LOSS)
        assertEquals(1, pauses)
        assertFalse(focus.hasFocus)
        assertFalse(focus.resumeOnGain)
        assertSame(listener, audio.lastAbandonedAudioFocusListener)

        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_GAIN)
        assertEquals(0, resumes)
        focus.close()
    }

    @Test
    fun focusChangesDoNotControlRemoteOrPausedPlayback() {
        val context = RuntimeEnvironment.getApplication()
        val audio = shadowOf(context.getSystemService(AudioManager::class.java))
        var pauses = 0
        var resumes = 0
        val focus = AndroidAudioFocus(context,
            isPlayingLocally = { false },
            pause = { pauses++ },
            resume = { resumes++ },
        )
        assertTrue(focus.acquire())
        val listener = audio.lastAudioFocusRequest.listener
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_LOSS_TRANSIENT)
        listener.onAudioFocusChange(AudioManager.AUDIOFOCUS_GAIN)
        assertEquals(0, pauses)
        assertEquals(0, resumes)
        assertFalse(focus.resumeOnGain)
        focus.close()
    }
}
