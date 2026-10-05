package io.github.screwys.rufin.player

import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import io.github.screwys.rufin.RufinService
import io.github.screwys.rufin.core.AndroidOutputKind
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidTransportState
import java.time.Duration
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSystemClock

@UnstableApi
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class RufinPlayerTest {
    private lateinit var player: RufinPlayer
    private val playing = AndroidPlaybackState(
        operationFailures = emptyList(),
        mediaUri = "file:///test/track.wav",
        mediaRun = 1UL,
        occurrenceId = "first",
        artworkIdentity = null,
        artworkRevision = 0UL,
        favorite = false,
        queueRevision = 1UL,
        queueWindowCount = 1UL,
        queueWindowRevision = 1UL,
        queueTotal = 1UL,
        queueIndex = 0UL,
        title = "Track",
        artist = "",
        album = "",
        contextId = null,
        contextTitle = null,
        contextCategory = null,
        contextKind = "queue",
        sourceFormat = "wav",
        bitrateKbps = null,
        bitDepth = null,
        sampleRateHz = 44_100U,
        positionMillis = 10_000UL,
        positionObservedAtMillis = 1UL,
        positionAdvancing = true,
        playbackRate = 1.0,
        durationMillis = 60_000UL,
        state = AndroidTransportState.PLAYING,
        desiredPlaying = true,
        canSeek = true,
        canNext = true,
        canPrevious = true,
        shuffle = false,
        autoDj = false,
        repeat = AndroidRepeatMode.OFF,
        volume = 1.0,
        localOutput = true,
        outputKind = AndroidOutputKind.LOCAL,
        outputId = "",
        outputName = null,
        error = null,
    )

    @Before
    fun setUp() {
        player = RufinPlayer(RufinService())
    }

    @After
    fun tearDown() {
        player.release()
    }

    @Test
    fun metadataAndFavoriteUpdatesPreserveThePositionClock() {
        player.update(playing)
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        player.update(playing.copy(favorite = true, title = "Updated track"))
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_500UL, player.positionNow())
        assertEquals(10_500L, player.currentPosition)

        player.update(playing.copy(favorite = true, queueRevision = 2UL))
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_750UL, player.positionNow())
        assertEquals(10_750L, player.currentPosition)
    }

    @Test
    fun pausedSnapshotsStayFixedAndResumeFromTheReportedPosition() {
        player.update(playing)
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        val paused = playing.copy(
            positionMillis = 10_250UL,
            positionObservedAtMillis = 2UL,
            positionAdvancing = false,
            state = AndroidTransportState.PAUSED,
            desiredPlaying = false,
        )
        player.update(paused)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        player.update(paused.copy(favorite = true, positionObservedAtMillis = 3UL))
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        player.update(paused.copy(
            positionObservedAtMillis = 4UL,
            positionAdvancing = true,
            playbackRate = 1.5,
            state = AndroidTransportState.PLAYING,
            desiredPlaying = true,
        ))
        ShadowSystemClock.advanceBy(Duration.ofMillis(400))
        assertEquals(10_850UL, player.positionNow())
        assertEquals(10_850L, player.currentPosition)
    }

    @Test
    fun repeatRunWaitsAtZeroForAnOutputPosition() {
        player.update(playing.copy(positionMillis = 59_000UL, repeat = AndroidRepeatMode.ONE))
        ShadowSystemClock.advanceBy(Duration.ofMillis(800))
        assertEquals(59_800UL, player.positionNow())

        val repeated = playing.copy(
            mediaRun = 2UL,
            positionMillis = 0UL,
            positionObservedAtMillis = 2UL,
            repeat = AndroidRepeatMode.ONE,
        )
        player.update(repeated)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(0UL, player.positionNow())
        assertEquals(0L, player.currentPosition)

        player.update(repeated.copy(positionMillis = 100UL, positionObservedAtMillis = 3UL))
        ShadowSystemClock.advanceBy(Duration.ofMillis(500))
        assertEquals(600UL, player.positionNow())
        assertEquals(600L, player.currentPosition)
    }

    @Test
    fun pauseFadeKeepsAdvancingUntilTheOutputStops() {
        player.update(playing)
        ShadowSystemClock.advanceBy(Duration.ofMillis(100))
        player.update(playing.copy(desiredPlaying = false))
        ShadowSystemClock.advanceBy(Duration.ofMillis(200))
        assertEquals(10_300UL, player.positionNow())

        player.update(playing.copy(
            desiredPlaying = false,
            positionMillis = 10_300UL,
            positionObservedAtMillis = 2UL,
            positionAdvancing = false,
            state = AndroidTransportState.PAUSED,
        ))
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_300UL, player.positionNow())
        assertEquals(10_300L, player.currentPosition)
        assertEquals(Player.STATE_READY, player.playbackState)
    }

    @Test
    fun remotePlaybackAdvancesFromZeroAndStopsAtTheDuration() {
        player.update(playing.copy(
            positionMillis = 0UL,
            playbackRate = 2.0,
            durationMillis = 1_500UL,
            localOutput = false,
            outputKind = AndroidOutputKind.GOOGLE_CAST,
            outputId = "remote",
        ))
        ShadowSystemClock.advanceBy(Duration.ofMillis(500))
        assertEquals(1_000UL, player.positionNow())
        assertEquals(1_000L, player.currentPosition)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(1_500UL, player.positionNow())
        assertEquals(1_500L, player.currentPosition)
    }
}
