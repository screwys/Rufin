package io.github.screwys.rufin.player

import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.ArtworkCache
import io.github.screwys.rufin.app.RufinConnection

import android.app.Application
import android.net.Uri
import android.os.SystemClock
import android.util.Log
import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidLyricsState
import io.github.screwys.rufin.core.AndroidPlayer
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.AndroidPlayerMetadata
import io.github.screwys.rufin.core.AndroidQueuePage
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidOutput
import io.github.screwys.rufin.core.AndroidOutputChoices
import io.github.screwys.rufin.core.AndroidPlayerSettings
import io.github.screwys.rufin.core.AndroidLyricsOperation
import io.github.screwys.rufin.core.AndroidLyricsDocument
import io.github.screwys.rufin.core.AndroidVisualizerDrawing
import io.github.screwys.rufin.core.AndroidRgb
import io.github.screwys.rufin.core.AndroidWaveform
import io.github.screwys.rufin.core.AndroidLyricsHighlight
import io.github.screwys.rufin.core.AndroidLyricsPosition
import io.github.screwys.rufin.core.AndroidTransportState
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import java.util.concurrent.Executors
import org.json.JSONObject

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class PlayerConnection(private val connection: RufinConnection) {
    internal val model get() = connection
    internal val artworkCache = ArtworkCache()
    private val readings = Executors.newSingleThreadExecutor { Thread(it, "Rufin lyrics") }.asCoroutineDispatcher()
    var settings by mutableStateOf<AndroidPlayerSettings?>(null)
        private set
    var lyricsOperation by mutableStateOf<AndroidLyricsOperation?>(null)
        private set
    var dictionaryState by mutableStateOf<String?>(null)
        private set
    var lyricsOffset by mutableLongStateOf(0L)
    private var sourceDocument: AndroidLyricsDocument? = null
    private var sourceOccurrence: String? = null
    internal var visualizerObservedAt = SystemClock.elapsedRealtimeNanos()
        private set
    var playback by mutableStateOf<AndroidPlaybackState?>(null)
        private set
    private var displayedArtwork by mutableStateOf<Pair<List<Byte>, PlayerArtwork>?>(null)
    val artwork: PlayerArtwork? get() = displayedArtwork?.second
    val displayedArtworkIdentity: List<Byte>? get() = displayedArtwork?.first
    var metadataLinks by mutableStateOf<AndroidPlayerMetadata?>(null)
        private set
    var outputChoices by mutableStateOf<AndroidOutputChoices?>(null)
        private set
    var waveform by mutableStateOf<AndroidWaveform?>(null)
        private set
    var lyrics by mutableStateOf<AndroidLyricsState?>(null)
        private set
    var levels by mutableStateOf<List<Double>>(emptyList())
        private set
    internal var bridge by mutableStateOf<AndroidPlayer?>(null)
        private set
    val service get() = connection.service
    val currentLyrics get() = lyrics?.takeIf { it.occurrenceId != null && it.occurrenceId == playback?.occurrenceId }
    val lyricsToken get() = lyrics?.currentMediaToken
    private fun showFeedback(message: String?) {
        message?.let { connection.showFeedback(it, control = true) }
    }

    fun positionNow(): ULong = service?.positionNow() ?: 0UL

    private suspend fun refreshLyrics(player: AndroidPlayer) {
        if (bridge !== player) return
        val projected = withContext(readings) { if (bridge === player) player.currentLyrics() else null }
        if (bridge === player) lyrics = projected
    }

    suspend fun observe() {
        try {
        snapshotFlow { connection.service }.collectLatest { service ->
            playback = null
            displayedArtwork = null
            metadataLinks = null
            if (service == null) return@collectLatest
            service.runtime.collectLatest { result ->
                val runtime = result?.getOrNull() ?: return@collectLatest
                val player = runtime.player()
                bridge = player
                outputChoices = null
                val lyricSubscription = player.subscribeLyrics()
                val visualizerSubscription = player.subscribeVisualizer()
                val operations = player.subscribeLyricsOperations()
                try {
                    settings = withContext(Dispatchers.IO) { player.settings() }
                    coroutineScope {
                        launch { while (isActive) waveform = player.nextWaveform() }
                        launch {
                            snapshotFlow { playback?.mediaUri }.collectLatest { uri ->
                                metadataLinks = null
                                if (uri == null) return@collectLatest
                                try {
                                    val loaded = player.metadataLinks(uri, null)
                                    if (bridge === player && playback?.mediaUri == uri) metadataLinks = loaded
                                } catch (cancelled: CancellationException) { throw cancelled }
                                catch (error: Exception) { reportError(error) }
                            }
                        }
                        launch {
                            while (isActive) {
                                val raw = lyricSubscription.next()
                                if (raw.occurrenceId != sourceOccurrence || raw.document != sourceDocument) lyricsOffset = 0L
                                sourceOccurrence = raw.occurrenceId
                                sourceDocument = raw.document
                                refreshLyrics(player)
                            }
                        }
                        launch {
                            while (isActive) {
                                levels = visualizerSubscription.next().levels
                                visualizerObservedAt = SystemClock.elapsedRealtimeNanos()
                            }
                        }
                        launch {
                            var savedRevision: ULong? = null
                            while (isActive) {
                                val update = operations.next()
                                if (dictionaryState != update.dictionary) {
                                    dictionaryState = update.dictionary
                                    if (update.dictionary == "ready") refreshLyrics(player)
                                }
                                update.search?.takeIf { it.mediaToken == lyricsToken }?.let { search ->
                                    if (search != lyricsOperation) {
                                        lyricsOperation = search
                                        search.message?.let { reportError(Exception(it)) }
                                    }
                                }
                                update.save?.takeIf { it.revision != savedRevision }?.let { save ->
                                    savedRevision = save.revision
                                    if (save.mediaToken == lyricsToken) {
                                        if (save.kind == "error") save.message?.let { reportError(Exception(it)) }
                                        else showFeedback(save.message)
                                    }
                                }
                            }
                        }
                        launch {
                            service.playback.collect {
                                val changed = it?.occurrenceId != playback?.occurrenceId || it?.mediaRun != playback?.mediaRun
                                val identity = it?.artworkIdentity?.toList()
                                it?.let { state -> artworkCache.updateRevision(state.artworkRevision) }
                                if (identity != null && identity != playback?.artworkIdentity?.toList())
                                    artworkCache.peek(identity, 1024)?.let { displayedArtwork = identity to it }
                                playback = it
                                if (changed) refreshLyrics(player)
                            }
                        }
                        snapshotFlow { playback?.let { state -> state.artworkIdentity?.toList()?.let { it to state.artworkRevision } } }.collectLatest { key ->
                            if (key == null) displayedArtwork = null
                            else {
                                val (identity, revision) = key
                                artworkCache.peek(identity, 1024)?.let { displayedArtwork = identity to it }
                                try {
                                    val loaded = artworkCache.load(player, identity, 1024)
                                    if (playback?.artworkIdentity?.toList() == identity && playback?.artworkRevision == revision)
                                        displayedArtwork = loaded?.let { identity to it }
                                }
                                catch (cancelled: CancellationException) { throw cancelled }
                                catch (error: Exception) { reportError(error) }
                            }
                        }
                    }
                } finally {
                    bridge = null
                    withContext(NonCancellable + readings) { player.releaseLyricsReader() }
                    settings = null
                    metadataLinks = null
                    waveform = null
                    lyricsOperation = null
                    dictionaryState = null
                    lyrics = null
                    levels = emptyList()
                    lyricSubscription.destroy()
                    visualizerSubscription.destroy()
                    operations.destroy()
                    player.destroy()
                }
            }
        }
        } finally { readings.close() }
    }

    private fun action(block: suspend () -> Unit) = connection.runAction(block)
    private suspend fun changeSettings(change: suspend (AndroidPlayer) -> AndroidPlayerSettings) {
        val current = bridge ?: return
        val projected = change(current)
        if (bridge === current) settings = projected
    }
    fun refreshSettings() = action { bridge?.let { settings = withContext(Dispatchers.IO) { it.settings() } } }
    fun equalizerEnabled(enabled: Boolean) = action { changeSettings { it.equalizerEnabled(enabled) } }
    fun equalizerPreset(preset: String) = action { changeSettings { it.equalizerPreset(preset) } }
    fun equalizerBand(index: Int, value: Float) = action { changeSettings { it.equalizerBand(index.toUInt(), value.toDouble()) } }
    fun lyricsPreference(field: String, value: Any?) = action {
        val current = bridge ?: return@action
        changeSettings { it.lyricsPreference(field, preferenceJson(value)) }
        refreshLyrics(current)
    }
    fun visualizerAppearance(field: String, value: Any?) = action {
        changeSettings { it.visualizerAppearanceField(field, preferenceJson(value)) }
    }
    fun visualizerFrameLimit(value: Int) = action { changeSettings { it.visualizerFrameLimit(value.toUInt()) } }
    fun visualizerPreset(slot: UInt, save: Boolean) = action { changeSettings { it.visualizerPreset(slot, save) } }
    fun resetVisualizer() = action { changeSettings { it.resetVisualizerAppearance() } }
    fun pasteVisualizer(text: String) = action { changeSettings { it.pasteVisualizerPreset(text) } }
    fun searchLyrics(token: String, artist: String, title: String) = action { bridge?.searchLyrics(token, artist, title) }
    fun previewLyrics(token: String, key: String) = action { bridge?.previewLyricsResult(token, key) }
    fun saveLyricsResult(token: String, key: String) = action { bridge?.saveLyricsResultToSource(token, key) }
    fun editLyrics(token: String, text: String) = action { bridge?.editLyrics(token, text) }
    fun saveLyrics(token: String) = action { bridge?.saveLyricsToSource(token, lyricsOffset) }
    fun clearLyrics(token: String) = action {
        val current = bridge ?: return@action
        changeSettings { it.clearFetchedLyrics(token) }
        refreshLyrics(current)
    }
    fun prepareDictionary() = action { bridge?.prepareLyricsDictionary(true) }
    suspend fun lyricsText(token: String): String? = withContext(readings) { bridge?.lyricsText(token, lyricsOffset) }
    fun exportLyrics(uri: Uri, token: String, offset: Long) = action {
        val text = withContext(readings) { bridge?.lyricsText(token, offset) } ?: return@action
        withContext(Dispatchers.IO) {
            val output = connection.getApplication<Application>().contentResolver.openOutputStream(uri, "wt")
                ?: error(translate("Save Lyrics"))
            output.bufferedWriter().use { it.write(text) }
        }
        showFeedback(translate("Saved"))
    }
    fun drawing(targets: List<Double>, width: Float, height: Float, frameNanos: Long, accent: AndroidRgb): AndroidVisualizerDrawing? =
        bridge?.visualizerDrawing(targets, width.toDouble(), height.toDouble(), frameNanos.toULong(), accent)

    private fun preferenceJson(value: Any?): String =
        if (value is String) JSONObject.quote(value) else JSONObject.wrap(value ?: JSONObject.NULL).toString()
    fun togglePlayback() = action { service?.let { if (playback?.desiredPlaying == true) it.pause() else it.play() } }
    fun next() {
        if (connection.debugLogging) Log.d("Rufin", "Player next clicked run=${playback?.mediaRun}")
        action { service?.next() }
    }
    fun previous() = action { service?.previous() }
    fun seek(millis: ULong) = action { service?.seekMillis(millis) }
    fun seekFromLyrics(token: String, millis: ULong) = action { bridge?.seekFromLyrics(token, millis, lyricsOffset) }
    fun favorite() {
        val current = playback ?: return
        val uri = current.mediaUri ?: return
        val effective = connection.projectedFavorite("track", uri, current.favorite)
        val favorite = !effective
        if (connection.debugLogging) Log.d("Rufin", "Player favorite clicked run=${current.mediaRun} effective=$effective requested=$favorite")
        connection.setFavoriteIntent("track", uri, favorite)
        playback = current.copy(favorite = favorite)
        action {
            try {
                val result = bridge?.setFavorite(uri, favorite)
                if (result != null && playback?.mediaUri == uri && projectedFavorite(uri, favorite) == favorite)
                    playback = playback?.copy(favorite = result)
            }
            finally {
                connection.releaseFavoriteIntent("track", uri, favorite)
            }
        }
    }
    fun shuffle() = action {
        val enabled = playback?.shuffle != true
        val current = service ?: return@action
        withContext(Dispatchers.IO) { current.setShuffle(enabled) }
    }
    fun autoDj() = action {
        val current = bridge ?: return@action
        withContext(Dispatchers.IO) { current.toggleAutoDj() }
    }
    fun repeat() = action {
        val next = when (playback?.repeat) {
            AndroidRepeatMode.OFF -> AndroidRepeatMode.ALL
            AndroidRepeatMode.ALL -> AndroidRepeatMode.ONE
            else -> AndroidRepeatMode.OFF
        }
        val current = service ?: return@action
        withContext(Dispatchers.IO) { current.setRepeat(next) }
    }
    suspend fun queuePage(offset: ULong, search: String = ""): AndroidQueuePage? = bridge?.queuePage(offset, 80u, search)
    suspend fun createQueuePlaylist(name: String, useCurrentSource: Boolean, public: Boolean?) {
        bridge?.createQueuePlaylist(name, useCurrentSource, public)
        connection.showFeedback(translate("Saved"))
    }
    suspend fun queueFavorite(mediaUri: String, favorite: Boolean): Boolean? {
        if (connection.debugLogging) Log.d("Rufin", "Queue favorite clicked run=${playback?.mediaRun} current=${playback?.mediaUri == mediaUri} requested=$favorite")
        connection.setFavoriteIntent("track", mediaUri, favorite)
        return try { bridge?.setFavorite(mediaUri, favorite) }
        finally {
            connection.releaseFavoriteIntent("track", mediaUri, favorite)
        }
    }
    internal fun projectedFavorite(uri: String, fallback: Boolean) = connection.projectedFavorite("track", uri, fallback)
    fun activateQueue(id: String) = action { bridge?.activateQueue(id) }
    fun removeQueue(id: String) = action { bridge?.removeQueue(id) }
    suspend fun moveQueue(id: String, target: String?, after: Boolean = false) {
        val current = bridge ?: return
        withContext(Dispatchers.IO) { current.moveQueue(id, target, after) }
    }
    fun clearQueue(includeCurrent: Boolean) = action { bridge?.clearQueue(includeCurrent) }
    fun visualizer(enabled: Boolean) = action { bridge?.setVisualizerEnabled(enabled) }
    suspend fun outputs(): AndroidOutputChoices? {
        val choices = bridge?.outputs()
        outputChoices = choices
        return choices
    }
    suspend fun selectOutput(output: AndroidOutput) { bridge?.selectOutput(output) }
    suspend fun selectAndroidOutput(routeId: String) {
        bridge?.selectLocalOutput()
        service?.outputs?.select(routeId)
    }
    fun volume(value: Double) = action { service?.setVolume(value) }
    fun reportError(error: Exception) = action { throw error }
}

@Composable
internal fun rememberPlayerConnection(connection: RufinConnection): PlayerConnection {
    val player = remember(connection) { PlayerConnection(connection) }
    LaunchedEffect(player) {
        try { player.observe() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
    }
    return player
}
