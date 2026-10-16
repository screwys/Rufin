package io.github.screwys.rufin.player

import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import io.github.screwys.rufin.RufinService
import io.github.screwys.rufin.core.AndroidPlaybackUpdate
import io.github.screwys.rufin.shared.playback.*
import io.github.screwys.rufin.shared.library.*
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
    private val row = object : QueueRow(io.github.screwys.rufin.shared.library.NoHandle) {
        override fun sameRow(other: QueueRow) = this === other
        override fun track() = TrackView(
            trackKey = 1L, sourceKey = null, sourceId = null, objectId = null,
            mediaUri = "file:///test/track.wav", artworkBinding = null,
            metadata = TrackMetadata(
                title = "Track", artist = "", album = "", albumDisplayArtist = null, sortText = "track",
                durationMillis = 60_000L, discNumber = 0L, trackNumber = 0L, year = null,
                releaseDate = null, dateAdded = null, sourceFormat = null,
                audioProperties = AudioProperties(null, null, null, null), comment = null, bpm = null,
                musicbrainzRecordingId = null, musicbrainzReleaseTrackId = null, musicbrainzAlbumId = null,
                musicbrainzReleaseGroupId = null, primaryArtistMusicbrainzId = null,
                artists = emptyList(), albumArtists = emptyList(), genres = emptyList(), moods = emptyList(),
                albumTags = null, sourceLoudness = null, albumLoudness = null,
            ),
            state = TrackState(false, null, null, 0L, 0L),
            access = TrackAccess(null, "", null, null, null, false, ByteArray(32)),
            links = TrackLinks(null, null, emptyList(), emptyList(), emptyList()),
        )
    }
    private val playing = AndroidPlaybackUpdate(
        queueLoading = false,
        queue = QueueSummaryView(1UL, 1UL, 0UL, null, true),
        notices = emptyList(), favorite = false, artworkRevision = 0UL, sourceLabels = listOf("WAV", "44.1 kHz"),
        transport = TransportView(
            current = CurrentMedia(null, CurrentMediaId(1UL, "file:///test/track.wav"), row),
            state = TransportStatus.PLAYING, desiredPlaying = true,
            positionMillis = 10_000UL, positionObservedAtMillis = 1UL, playbackRate = 1.0,
            durationMillis = 60_000UL, canSeek = true, bufferingPercent = null, error = null,
        ),
        controls = ControlsView(QueueRepeatMode.OFF, false, false, 1.0, false, null, PlaybackOutput.Local),
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
        assertEquals("run:1", player.currentMediaItem?.mediaId)
        assertEquals(null, player.currentMediaItem?.localConfiguration)
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        val updatedRow = object : QueueRow(io.github.screwys.rufin.shared.library.NoHandle) {
            override fun sameRow(other: QueueRow) = this === other
            override fun track() = row.track().let { it.copy(metadata = it.metadata.copy(title = "Updated track")) }
        }
        val updated = playing.copy(favorite = true,
            transport = playing.transport.copy(current = playing.transport.current!!.copy(row = updatedRow)))
        player.update(updated)
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_500UL, player.positionNow())
        assertEquals(10_500L, player.currentPosition)

        player.update(updated.copy(queue = playing.queue.copy(revision = 2UL)))
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        assertEquals(10_750UL, player.positionNow())
        assertEquals(10_750L, player.currentPosition)
    }

    @Test
    fun pausedSnapshotsStayFixedAndResumeFromTheReportedPosition() {
        player.update(playing)
        ShadowSystemClock.advanceBy(Duration.ofMillis(250))
        val paused = playing.copy(transport = playing.transport.copy(
            positionMillis = 10_250UL,
            positionObservedAtMillis = 2UL,
            state = TransportStatus.PAUSED,
            desiredPlaying = false,
        ))
        player.update(paused)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        player.update(paused.copy(transport = paused.transport.copy(positionObservedAtMillis = 3UL)))
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_250UL, player.positionNow())
        assertEquals(10_250L, player.currentPosition)

        player.update(paused.copy(transport = paused.transport.copy(
            positionObservedAtMillis = 4UL,
            playbackRate = 1.5,
            state = TransportStatus.PLAYING,
            desiredPlaying = true,
        )))
        ShadowSystemClock.advanceBy(Duration.ofMillis(400))
        assertEquals(10_850UL, player.positionNow())
        assertEquals(10_850L, player.currentPosition)
    }

    @Test
    fun repeatRunWaitsAtZeroForAnOutputPosition() {
        player.update(playing.copy(transport = playing.transport.copy(positionMillis = 59_000UL), controls = playing.controls.copy(repeatMode = QueueRepeatMode.ONE)))
        ShadowSystemClock.advanceBy(Duration.ofMillis(800))
        assertEquals(59_800UL, player.positionNow())

        val repeated = playing.copy(
            transport = playing.transport.copy(
                current = playing.transport.current!!.copy(id = playing.transport.current!!.id.copy(run = 2UL)),
                positionMillis = 0UL, positionObservedAtMillis = 2UL,
            ),
            controls = playing.controls.copy(repeatMode = QueueRepeatMode.ONE),
        )
        player.update(repeated)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(0UL, player.positionNow())
        assertEquals(0L, player.currentPosition)

        player.update(repeated.copy(transport = repeated.transport.copy(positionMillis = 100UL, positionObservedAtMillis = 3UL)))
        ShadowSystemClock.advanceBy(Duration.ofMillis(500))
        assertEquals(600UL, player.positionNow())
        assertEquals(600L, player.currentPosition)
    }

    @Test
    fun pauseFadeKeepsAdvancingUntilTheOutputStops() {
        player.update(playing)
        ShadowSystemClock.advanceBy(Duration.ofMillis(100))
        player.update(playing.copy(transport = playing.transport.copy(desiredPlaying = false)))
        ShadowSystemClock.advanceBy(Duration.ofMillis(200))
        assertEquals(10_300UL, player.positionNow())

        player.update(playing.copy(transport = playing.transport.copy(
            desiredPlaying = false,
            positionMillis = 10_300UL,
            positionObservedAtMillis = 2UL,
            state = TransportStatus.PAUSED,
        )))
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(10_300UL, player.positionNow())
        assertEquals(10_300L, player.currentPosition)
        assertEquals(Player.STATE_READY, player.playbackState)
    }

    @Test
    fun remotePlaybackAdvancesFromZeroAndStopsAtTheDuration() {
        player.update(playing.copy(
            transport = playing.transport.copy(positionMillis = 0UL, playbackRate = 2.0, durationMillis = 1_500UL),
            controls = playing.controls.copy(playbackOutput = PlaybackOutput.Remote(RemoteOutput("remote", "Remote", RemoteOutputProtocol.GOOGLE_CAST))),
        ))
        ShadowSystemClock.advanceBy(Duration.ofMillis(500))
        assertEquals(1_000UL, player.positionNow())
        assertEquals(1_000L, player.currentPosition)
        ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
        assertEquals(1_500UL, player.positionNow())
        assertEquals(1_500L, player.currentPosition)
    }
}
