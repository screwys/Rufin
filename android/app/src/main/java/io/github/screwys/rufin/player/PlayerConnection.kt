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
import io.github.screwys.rufin.shared.rufin_core.PlayerPreferences
import io.github.screwys.rufin.shared.rufin_core.PlayerPreferenceOptions
import io.github.screwys.rufin.shared.rufin_core.PlayerPreferenceEdit
import io.github.screwys.rufin.shared.rufin_core.VisualizerPreferenceEdit
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.shared.lyrics.LyricsPreferenceEdit
import io.github.screwys.rufin.core.AndroidLyricsOperation
import io.github.screwys.rufin.core.AndroidLyricsSource
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

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class PlayerConnection(private val connection: RufinConnection) {
    internal val model get() = connection
    internal val artworkCache = ArtworkCache()
    private val readings = Executors.newSingleThreadExecutor { Thread(it, "Rufin lyrics") }.asCoroutineDispatcher()
    var settings by mutableStateOf<PlayerPreferences?>(null)
        private set
    var preferenceOptions by mutableStateOf<PlayerPreferenceOptions?>(null)
        private set
    private var settingsOwner: SettingsOwner? = null
    var lyricsOperation by mutableStateOf<AndroidLyricsOperation?>(null)
        private set
    var dictionaryState by mutableStateOf<String?>(null)
        private set
    var lyricsOffset by mutableLongStateOf(0L)
    private var sourceLyrics: AndroidLyricsSource? = null
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
    val currentLyrics get() = lyrics?.takeIf { it.mediaUri != null && it.mediaUri == playback?.mediaUri && it.mediaRun == playback?.mediaRun }
    val lyricsToken get() = lyrics?.currentMediaToken
    private fun showFeedback(message: String?) {
        message?.let { connection.showFeedback(it, control = true) }
    }

    fun positionNow(): ULong = service?.positionNow() ?: 0UL

    private suspend fun refreshLyrics(player: AndroidPlayer) {
        if (bridge !== player) return
        val source = sourceLyrics ?: return
        val projected = withContext(readings) {
            if (bridge === player && sourceLyrics === source) player.currentLyrics(source) else null
        }
        if (bridge === player && sourceLyrics === source) lyrics = projected
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
                val owner = player.settingsOwner()
                settingsOwner = owner
                outputChoices = null
                val lyricSubscription = player.subscribeLyrics()
                val visualizerSubscription = player.subscribeVisualizer()
                val operations = player.subscribeLyricsOperations()
                try {
                    withContext(Dispatchers.IO) { owner.playerPreferenceOptions() }.let { preferenceOptions = it }
                    settings = withContext(Dispatchers.IO) { owner.playerPreferences() }
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
                                val previous = sourceLyrics
                                if (raw.documentChanged(previous)) lyricsOffset = 0L
                                sourceLyrics = raw
                                withContext(NonCancellable + readings) { previous?.destroy() }
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
                                val changed = it?.mediaUri != playback?.mediaUri || it?.mediaRun != playback?.mediaRun
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
                    settingsOwner = null
                    owner.destroy()
                    settings = null
                    preferenceOptions = null
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
        } finally {
            val source = sourceLyrics
            sourceLyrics = null
            withContext(NonCancellable + readings) { source?.destroy() }
            readings.close()
        }
    }

    private fun action(block: suspend () -> Unit) = connection.runAction(block)
    private suspend fun changeSettings(change: (SettingsOwner) -> Unit) {
        val current = settingsOwner ?: return
        val projected = withContext(Dispatchers.IO) {
            change(current)
            current.playerPreferences()
        }
        if (settingsOwner === current) settings = projected
    }
    fun refreshSettings() = action { changeSettings {} }
    fun editPreferences(edit: PlayerPreferenceEdit) = action { changeSettings { it.editPlayerPreferences(edit) } }
    fun equalizerEnabled(enabled: Boolean) = action { changeSettings { it.setEqualizerEnabled(enabled) } }
    fun equalizerPreset(preset: String) = action { changeSettings { it.selectEqualizerPreset(preset) } }
    fun equalizerBand(index: Int, value: Float) = action { changeSettings { it.setEqualizerBand(index.toULong(), value.toDouble()) } }
    fun lyricsPreference(edit: LyricsPreferenceEdit) = action {
        val current = bridge ?: return@action
        changeSettings { it.editLyricsPreferences(edit) }
        refreshLyrics(current)
    }
    fun visualizerAppearance(edit: VisualizerPreferenceEdit) = action { changeSettings { it.editVisualizerPreferences(edit) } }
    fun visualizerFrameLimit(value: Int) = action { changeSettings { it.setVisualizerFrameLimit(value.toUInt()) } }
    fun visualizerPreset(slot: ULong, save: Boolean) = action { changeSettings { it.updateVisualizerPreset(slot, save) } }
    fun resetVisualizer() = action { changeSettings { it.resetVisualizerAppearance() } }
    fun pasteVisualizer(text: String) = action { changeSettings { it.pasteVisualizerPreset(text) } }
    fun copyVisualizer(copy: (String) -> Unit) = action {
        val current = settingsOwner ?: return@action
        val text = withContext(Dispatchers.IO) { current.copyVisualizerPreset() }
        copy(text)
    }
    fun searchLyrics(token: String, artist: String, title: String) = action { bridge?.searchLyrics(token, artist, title) }
    fun previewLyrics(token: String, key: String) = action { bridge?.previewLyricsResult(token, key) }
    fun saveLyricsResult(token: String, key: String) = action { bridge?.saveLyricsResultToSource(token, key) }
    fun editLyrics(token: String, text: String) = action { bridge?.editLyrics(token, text) }
    fun saveLyrics(token: String) = action { bridge?.saveLyricsToSource(token, lyricsOffset) }
    fun clearLyrics(token: String) = action {
        val current = bridge ?: return@action
        current.clearFetchedLyrics(token)
        changeSettings {}
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
    fun activateQueue(position: ULong) = action { bridge?.activateQueue(position) }
    fun removeQueue(position: ULong) = action { bridge?.removeQueue(position) }
    suspend fun moveQueue(position: ULong, target: ULong?, after: Boolean = false) {
        val current = bridge ?: return
        withContext(Dispatchers.IO) { current.moveQueue(position, target, after) }
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
