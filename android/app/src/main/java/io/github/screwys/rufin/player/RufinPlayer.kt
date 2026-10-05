package io.github.screwys.rufin.player

import io.github.screwys.rufin.RufinService

import android.os.Looper
import android.os.SystemClock
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.DeviceInfo
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.SimpleBasePlayer
import androidx.media3.common.util.UnstableApi
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidTransportState

/** Maps shared playback state and commands to Android media controls. */
@UnstableApi
internal class RufinPlayer(private val service: RufinService) : SimpleBasePlayer(Looper.getMainLooper()) {
    private var playback: AndroidPlaybackState? = null
    private var positionObservedAt = SystemClock.elapsedRealtime()
    private val positionSupplier = PositionSupplier { positionNow().toLong() }
    private var commandError: PlaybackException? = null
    private var playbackError: PlaybackException? = null
    private var metadata = MediaMetadata.EMPTY

    fun update(value: AndroidPlaybackState?) {
        if (value?.positionObservedAtMillis != playback?.positionObservedAtMillis ||
            value?.positionMillis != playback?.positionMillis || value?.positionAdvancing != playback?.positionAdvancing ||
            value?.playbackRate != playback?.playbackRate ||
            value?.occurrenceId != playback?.occurrenceId || value?.mediaRun != playback?.mediaRun ||
            value?.outputId != playback?.outputId || value?.outputKind != playback?.outputKind) {
            positionObservedAt = SystemClock.elapsedRealtime()
        }
        val artworkChanged = value?.artworkIdentity?.toList() != playback?.artworkIdentity?.toList()
        if (artworkChanged || value?.title != playback?.title || value?.artist != playback?.artist || value?.album != playback?.album) {
            metadata = metadata.buildUpon().setTitle(value?.title).setArtist(value?.artist).setAlbumTitle(value?.album)
                .apply { if (artworkChanged) setArtworkData(null, MediaMetadata.PICTURE_TYPE_FRONT_COVER) }.build()
        }
        if (value?.error != playback?.error) {
            playbackError = value?.error?.let { PlaybackException(it, null, PlaybackException.ERROR_CODE_UNSPECIFIED) }
        }
        playback = value
        invalidateState()
    }

    fun positionNow(): ULong {
        val current = playback ?: return 0UL
        // A local handoff reports zero before the new audio reaches the output.
        val elapsed = if (current.positionAdvancing &&
            (!current.localOutput || current.positionObservedAtMillis == 0UL || current.positionMillis != 0UL))
            (SystemClock.elapsedRealtime() - positionObservedAt).coerceAtLeast(0) else 0L
        val position = (current.positionMillis.toDouble() + elapsed * current.playbackRate).toULong()
        return if (current.durationMillis > 0UL) position.coerceAtMost(current.durationMillis) else position
    }

    fun updateArtwork(identity: List<Byte>, revision: ULong, bytes: ByteArray?) {
        if (playback?.artworkIdentity?.toList() != identity || playback?.artworkRevision != revision) return
        metadata = metadata.buildUpon().setArtworkData(bytes, MediaMetadata.PICTURE_TYPE_FRONT_COVER).build()
        invalidateState()
    }

    fun clearCommandError() {
        if (commandError == null) return
        commandError = null
        invalidateState()
    }

    fun failed(error: Exception) {
        commandError = PlaybackException(error.message, error, PlaybackException.ERROR_CODE_UNSPECIFIED)
        invalidateState()
    }

    override fun getState(): State {
        val current = playback
        val commands = Player.Commands.Builder().add(Player.COMMAND_RELEASE)
        if (current != null) {
            commands.addAll(
                Player.COMMAND_PLAY_PAUSE,
                Player.COMMAND_STOP,
                Player.COMMAND_GET_CURRENT_MEDIA_ITEM,
                Player.COMMAND_GET_TIMELINE,
                Player.COMMAND_GET_METADATA,
                Player.COMMAND_GET_AUDIO_ATTRIBUTES,
                Player.COMMAND_GET_VOLUME,
                Player.COMMAND_SET_VOLUME,
                Player.COMMAND_SET_REPEAT_MODE,
                Player.COMMAND_SET_SHUFFLE_MODE,
            )
            if (current.canSeek) commands.addAll(
                Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM,
                Player.COMMAND_SEEK_TO_DEFAULT_POSITION,
                Player.COMMAND_SEEK_BACK,
                Player.COMMAND_SEEK_FORWARD,
            )
            if (current.canNext) commands.addAll(Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM)
            if (current.canPrevious) commands.addAll(Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM)
        }
        val error = playbackError ?: commandError
        val state = State.Builder()
            .setAvailableCommands(commands.build())
            .setAudioAttributes(AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MUSIC).build())
            .setDeviceInfo(DeviceInfo.Builder(if (current?.localOutput == false) DeviceInfo.PLAYBACK_TYPE_REMOTE else DeviceInfo.PLAYBACK_TYPE_LOCAL).build())
            .setPlayWhenReady(current?.desiredPlaying == true, Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST)
            .setPlaybackState(if (error != null || current?.mediaUri == null) Player.STATE_IDLE else when (current.state) {
                AndroidTransportState.BUFFERING -> Player.STATE_BUFFERING
                AndroidTransportState.PLAYING, AndroidTransportState.PAUSED -> Player.STATE_READY
                else -> Player.STATE_IDLE
            })
            .setPlayerError(error)
            .setPlaybackParameters(PlaybackParameters(current?.playbackRate?.toFloat() ?: 1f))
            .setVolume(current?.volume?.toFloat()?.coerceIn(0f, 1f) ?: 1f)
            .setShuffleModeEnabled(current?.shuffle == true)
            .setRepeatMode(when (current?.repeat) {
                AndroidRepeatMode.ALL -> Player.REPEAT_MODE_ALL
                AndroidRepeatMode.ONE -> Player.REPEAT_MODE_ONE
                else -> Player.REPEAT_MODE_OFF
            })
        current?.mediaUri?.let { uri ->
            val item = MediaItem.Builder().setMediaId(uri).setUri(uri).setMediaMetadata(metadata).build()
            state.setPlaylist(listOf(MediaItemData.Builder(uri)
                .setMediaItem(item)
                .setIsSeekable(current.canSeek)
                .setDurationUs(if (current.durationMillis == 0UL) C.TIME_UNSET else current.durationMillis.toLong() * 1000)
                .build()))
                .setCurrentMediaItemIndex(0)
                .setContentPositionMs(positionSupplier)
        }
        return state.build()
    }

    private inline fun command(action: () -> Unit): ListenableFuture<*> = try {
        commandError = null
        action()
        Futures.immediateVoidFuture()
    } catch (error: Exception) {
        failed(error)
        Futures.immediateFailedFuture<Void>(error)
    }

    override fun handleSetPlayWhenReady(playWhenReady: Boolean) = command {
        if (playWhenReady) service.play() else service.pause()
    }

    override fun handleStop() = command { service.stop() }

    override fun handleRelease(): ListenableFuture<*> = Futures.immediateVoidFuture()

    override fun handleSeek(mediaItemIndex: Int, positionMs: Long, seekCommand: Int) = command {
        // Rust owns queue navigation, including restart-versus-previous behavior.
        when (seekCommand) {
            Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM -> service.next()
            Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM -> service.previous()
            else -> service.seekMillis(if (positionMs == C.TIME_UNSET) 0UL else positionMs.coerceAtLeast(0).toULong())
        }
    }

    override fun handleSetVolume(volume: Float, volumeOperationType: Int) = command { service.setVolume(volume.toDouble()) }

    override fun handleSetShuffleModeEnabled(shuffleModeEnabled: Boolean) = command { service.setShuffle(shuffleModeEnabled) }

    override fun handleSetRepeatMode(repeatMode: Int) = command {
        service.setRepeat(when (repeatMode) {
            Player.REPEAT_MODE_ALL -> AndroidRepeatMode.ALL
            Player.REPEAT_MODE_ONE -> AndroidRepeatMode.ONE
            else -> AndroidRepeatMode.OFF
        })
    }
}
