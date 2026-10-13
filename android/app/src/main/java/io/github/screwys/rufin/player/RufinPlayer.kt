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
import io.github.screwys.rufin.shared.playback.*
import io.github.screwys.rufin.core.AndroidPlaybackUpdate
import io.github.screwys.rufin.shared.library.QueueItem
import io.github.screwys.rufin.shared.library.QueueRepeatMode

/** Maps shared playback state and commands to Android media controls. */
@UnstableApi
internal class RufinPlayer(private val service: RufinService) : SimpleBasePlayer(Looper.getMainLooper()) {
    private var playback: AndroidPlaybackUpdate? = null
    private var item: QueueItem? = null
    private var artworkRevision = 0UL
    var artworkIdentity: List<Byte>? = null
        private set
    private var positionObservedAt = SystemClock.elapsedRealtime()
    private val positionSupplier = PositionSupplier { positionNow().toLong() }
    private var commandError: PlaybackException? = null
    private var playbackError: PlaybackException? = null
    private var metadata = MediaMetadata.EMPTY

    fun update(value: AndroidPlaybackUpdate?) {
        val previous = playback
        val transport = value?.transport
        val before = previous?.transport
        if (transport?.positionObservedAtMillis != before?.positionObservedAtMillis ||
            transport?.positionMillis != before?.positionMillis ||
            (transport?.state == TransportStatus.PLAYING) != (before?.state == TransportStatus.PLAYING) ||
            transport?.playbackRate != before?.playbackRate || transport?.current?.id != before?.current?.id ||
            value?.controls?.playbackOutput != previous?.controls?.playbackOutput) {
            positionObservedAt = SystemClock.elapsedRealtime()
        }
        val row = value?.transport?.current?.row
        val previousRow = previous?.transport?.current?.row
        val rowChanged = if (row == null) previousRow != null else previousRow == null ||
            row !== previousRow && !row.sameRow(previousRow)
        val nextItem = if (rowChanged) row?.item() else item
        val nextIdentity = if (rowChanged) nextItem?.artworkBinding?.toList() else artworkIdentity
        val artworkChanged = nextIdentity != artworkIdentity
        if (artworkChanged || nextItem?.title != item?.title || nextItem?.artist != item?.artist || nextItem?.album != item?.album) {
            metadata = metadata.buildUpon().setTitle(nextItem?.title).setArtist(nextItem?.artist).setAlbumTitle(nextItem?.album)
                .apply { if (artworkChanged) setArtworkData(null, MediaMetadata.PICTURE_TYPE_FRONT_COVER) }.build()
        }
        if (value?.transport?.error != previous?.transport?.error) {
            playbackError = value?.transport?.error?.let { PlaybackException(it, null, PlaybackException.ERROR_CODE_UNSPECIFIED) }
        }
        item = nextItem
        artworkIdentity = nextIdentity
        artworkRevision = value?.artworkRevision ?: 0UL
        playback = value
        invalidateState()
    }

    fun positionNow(): ULong {
        val current = playback ?: return 0UL
        // A local handoff reports zero before the new audio reaches the output.
        val elapsed = if ((current.transport.state == TransportStatus.PLAYING) &&
            (current.controls.playbackOutput !is PlaybackOutput.Local || current.transport.positionObservedAtMillis == 0UL || current.transport.positionMillis != 0UL))
            (SystemClock.elapsedRealtime() - positionObservedAt).coerceAtLeast(0) else 0L
        val position = (current.transport.positionMillis.toDouble() + elapsed * current.transport.playbackRate).toULong()
        return if (current.transport.durationMillis > 0UL) position.coerceAtMost(current.transport.durationMillis) else position
    }

    fun updateArtwork(identity: List<Byte>, revision: ULong, bytes: ByteArray?) {
        if (artworkIdentity != identity || artworkRevision != revision) return
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
            if (current.transport.canSeek) commands.addAll(
                Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM,
                Player.COMMAND_SEEK_TO_DEFAULT_POSITION,
                Player.COMMAND_SEEK_BACK,
                Player.COMMAND_SEEK_FORWARD,
            )
            if (current.queue.canNext) commands.addAll(Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM)
            if ((current.transport.current != null)) commands.addAll(Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM)
        }
        val error = playbackError ?: commandError
        val state = State.Builder()
            .setAvailableCommands(commands.build())
            .setAudioAttributes(AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MUSIC).build())
            .setDeviceInfo(DeviceInfo.Builder(if (current?.controls?.playbackOutput is PlaybackOutput.Remote) DeviceInfo.PLAYBACK_TYPE_REMOTE else DeviceInfo.PLAYBACK_TYPE_LOCAL).build())
            .setPlayWhenReady(current?.transport?.desiredPlaying == true, Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST)
            .setPlaybackState(if (error != null || current?.transport?.current?.id?.mediaUri == null) Player.STATE_IDLE else when {
                current.transport.state == TransportStatus.STOPPED || current.transport.state == TransportStatus.FAILED -> Player.STATE_IDLE
                !current.transport.desiredPlaying || current.transport.state == TransportStatus.PLAYING -> Player.STATE_READY
                else -> Player.STATE_BUFFERING
            })
            .setPlayerError(error)
            .setPlaybackParameters(PlaybackParameters(current?.transport?.playbackRate?.toFloat() ?: 1f))
            .setVolume(current?.controls?.volume?.toFloat()?.coerceIn(0f, 1f) ?: 1f)
            .setShuffleModeEnabled(current?.controls?.shuffleEnabled == true)
            .setRepeatMode(when (current?.controls?.repeatMode) {
                QueueRepeatMode.ALL -> Player.REPEAT_MODE_ALL
                QueueRepeatMode.ONE -> Player.REPEAT_MODE_ONE
                else -> Player.REPEAT_MODE_OFF
            })
        current?.transport?.current?.id?.mediaUri?.let { uri ->
            val item = MediaItem.Builder().setMediaId(uri).setUri(uri).setMediaMetadata(metadata).build()
            state.setPlaylist(listOf(MediaItemData.Builder(uri)
                .setMediaItem(item)
                .setIsSeekable(current.transport.canSeek)
                .setDurationUs(if (current.transport.durationMillis == 0UL) C.TIME_UNSET else current.transport.durationMillis.toLong() * 1000)
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
            Player.REPEAT_MODE_ALL -> QueueRepeatMode.ALL
            Player.REPEAT_MODE_ONE -> QueueRepeatMode.ONE
            else -> QueueRepeatMode.OFF
        })
    }
}
