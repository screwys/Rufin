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
import io.github.screwys.rufin.core.AndroidPlaybackUpdate
import io.github.screwys.rufin.core.AndroidPlayerMetadata
import io.github.screwys.rufin.shared.rufin_core.PlayerPreferences
import io.github.screwys.rufin.shared.rufin_core.PlayerPreferenceOptions
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.core.AndroidVisualizerDrawing
import io.github.screwys.rufin.core.AndroidRgb
import io.github.screwys.rufin.core.AndroidLyricsHighlight
import io.github.screwys.rufin.core.AndroidLyricsPosition
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.playback.*
import io.github.screwys.rufin.shared.library.QueueItem
import io.github.screwys.rufin.shared.library.FavoriteTarget
import io.github.screwys.rufin.shared.library.QueueReorderTarget
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.lyrics.*
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
    var lyricsSearch by mutableStateOf<Pair<CurrentMediaId, LyricsQuery>?>(null)
        private set
    var lyricsResults by mutableStateOf<List<LyricsSearchResult>?>(null)
        private set
    var lyricsSearching by mutableStateOf(false)
        private set
    private var playbackOwner: PlaybackOwner? = null
    private var lyricsOwner: LyricsHandle? = null
    private var sourceOwner: SourceOwner? = null
    var dictionaryState by mutableStateOf<JapaneseDictionaryStatus?>(null)
        private set
    var lyricsOffset by mutableLongStateOf(0L)
    private var sourceLyrics: CurrentLyrics? = null
    internal var visualizerObservedAt = SystemClock.elapsedRealtimeNanos()
        private set
    var playback by mutableStateOf<AndroidPlaybackUpdate?>(null)
        private set
    var currentItem by mutableStateOf<QueueItem?>(null)
        private set
    var favorite by mutableStateOf(false)
        private set
    var artworkRevision by mutableStateOf(0UL)
        private set
    var sourceLabels by mutableStateOf<List<String>>(emptyList())
        private set
    var currentContextId by mutableStateOf<String?>(null)
        private set
    var currentSourceRank by mutableStateOf<ULong?>(null)
        private set
    var currentContext by mutableStateOf<Triple<String?, String, String?>?>(null)
        private set
    val playingMediaUri by derivedStateOf(structuralEqualityPolicy()) {
        playback?.transport?.takeIf { it.desiredPlaying }?.current?.id?.mediaUri
    }
    val currentId get() = playback?.transport?.current?.id
    private var displayedArtwork by mutableStateOf<Pair<List<Byte>, PlayerArtwork>?>(null)
    val artwork: PlayerArtwork? get() = displayedArtwork?.second
    val displayedArtworkIdentity: List<Byte>? get() = displayedArtwork?.first
    var metadataLinks by mutableStateOf<AndroidPlayerMetadata?>(null)
        private set
    var outputChoices by mutableStateOf<List<RemoteOutput>>(emptyList())
        private set
    var waveform by mutableStateOf<Pair<CurrentMediaId?, List<Double>>?>(null)
        private set
    var lyrics by mutableStateOf<AndroidLyricsState?>(null)
        private set
    var levels by mutableStateOf<List<Double>>(emptyList())
        private set
    internal var bridge by mutableStateOf<AndroidPlayer?>(null)
        private set
    val service get() = connection.service
    val currentLyrics get() = lyrics?.takeIf { it.mediaId != null && it.mediaId == currentId }
    val lyricsToken get() = lyrics?.currentMediaId
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
            currentItem = null
            currentContextId = null
            currentSourceRank = null
            currentContext = null
            displayedArtwork = null
            metadataLinks = null
            if (service == null) return@collectLatest
            service.runtime.collectLatest { result ->
                val runtime = result?.getOrNull() ?: return@collectLatest
                val player = runtime.player()
                bridge = player
                playbackOwner = runtime.playbackOwner()
                lyricsOwner = runtime.lyricsOwner()
                sourceOwner = runtime.source()
                val owner = runtime.settings()
                settingsOwner = owner
                outputChoices = emptyList()
                val lyricSubscription = player.subscribeLyrics()
                val visualizerSubscription = player.subscribeVisualizer()
                try {
                    withContext(Dispatchers.IO) { owner.playerPreferenceOptions() }.let { preferenceOptions = it }
                    settings = withContext(Dispatchers.IO) { owner.playerPreferences() }
                    coroutineScope {
                        launch { while (isActive) {
                            val frame = player.nextWaveform()
                            try { waveform = frame.mediaId() to frame.amplitudes() } finally { frame.destroy() }
                        } }
                        launch {
                            snapshotFlow { currentId?.mediaUri }.collectLatest { uri ->
                                metadataLinks = null
                                if (uri == null) return@collectLatest
                                try {
                                    val loaded = player.metadataLinks(uri, null)
                                    if (bridge === player && currentId?.mediaUri == uri) metadataLinks = loaded
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
                                levels = visualizerSubscription.next()
                                visualizerObservedAt = SystemClock.elapsedRealtimeNanos()
                            }
                        }
                        launch {
                            while (isActive) {
                                val event = player.nextLyricsEvent()
                                try {
                                    event.dictionaryStatus()?.let { status ->
                                        dictionaryState = status
                                        if (status is JapaneseDictionaryStatus.Ready) refreshLyrics(player)
                                    }
                                    val media = event.mediaId()
                                    val query = event.query()
                                    if (media == lyricsToken) {
                                        if (query != null && lyricsSearch == (media to query)) {
                                            lyricsResults = if (event.error() == null) event.results() else null
                                            lyricsSearching = false
                                        }
                                        event.error()?.let { reportError(Exception(it)) }
                                        if (event.saved()) showFeedback(translate("Saved"))
                                    }
                                } finally { event.destroy() }
                            }
                        }
                        launch {
                            service.playback.collect { update ->
                                val changed = update?.transport?.current?.id != currentId
                                val row = update?.transport?.current?.row
                                val previousRow = playback?.transport?.current?.row
                                val rowChanged = if (row == null) previousRow != null else previousRow == null ||
                                    row !== previousRow && !row.sameRow(previousRow)
                                if (rowChanged) {
                                    currentItem = row?.item()
                                    currentContextId = row?.contextId()
                                    currentSourceRank = row?.sourceRank()
                                    currentContext = row?.let { Triple(it.contextTitle(), it.contextKind(), it.contextCategory()) }
                                }
                                val identity = currentItem?.artworkBinding?.toList()
                                artworkRevision = update?.artworkRevision ?: 0UL
                                artworkCache.updateRevision(artworkRevision)
                                if (identity != null && rowChanged)
                                    artworkCache.peek(identity, 1024)?.let { displayedArtwork = identity to it }
                                favorite = update?.favorite ?: false
                                sourceLabels = update?.sourceLabels.orEmpty()
                                playback = update
                                if (changed) refreshLyrics(player)
                            }
                        }
                        snapshotFlow { currentItem?.artworkBinding?.toList()?.let { it to artworkRevision } }.collectLatest { key ->
                            if (key == null) displayedArtwork = null
                            else {
                                val (identity, revision) = key
                                artworkCache.peek(identity, 1024)?.let { displayedArtwork = identity to it }
                                try {
                                    val loaded = artworkCache.load(player, identity, 1024)
                                    if (currentItem?.artworkBinding?.toList() == identity && artworkRevision == revision)
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
                    lyricsSearch = null
                    lyricsResults = null
                    lyricsSearching = false
                    dictionaryState = null
                    lyrics = null
                    levels = emptyList()
                    lyricSubscription.destroy()
                    visualizerSubscription.destroy()
                    playbackOwner?.destroy()
                    playbackOwner = null
                    lyricsOwner?.destroy()
                    lyricsOwner = null
                    sourceOwner?.destroy()
                    sourceOwner = null
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
    fun editPreferences(edit: SettingsOwner.() -> Unit) = action { changeSettings(edit) }
    fun equalizerEnabled(enabled: Boolean) = action { changeSettings { it.setEqualizerEnabled(enabled) } }
    fun equalizerPreset(preset: String) = action { changeSettings { it.selectEqualizerPreset(preset) } }
    fun equalizerBand(index: Int, value: Float) = action { changeSettings { it.setEqualizerBand(index.toULong(), value.toDouble()) } }
    fun lyricsPreference(edit: SettingsOwner.() -> Unit) = action {
        val current = bridge ?: return@action
        changeSettings(edit)
        refreshLyrics(current)
    }
    fun visualizerAppearance(edit: SettingsOwner.() -> Unit) = action { changeSettings(edit) }
    fun visualizerFrameLimit(value: Int) = action { changeSettings { it.setVisualizerFrameLimit(value.toUInt()) } }
    fun visualizerPreset(slot: ULong, save: Boolean) = action { changeSettings { it.updateVisualizerPreset(slot, save) } }
    fun resetVisualizer() = action { changeSettings { it.resetVisualizerAppearance() } }
    fun pasteVisualizer(text: String) = action { changeSettings { it.pasteVisualizerPreset(text) } }
    fun copyVisualizer(copy: (String) -> Unit) = action {
        val current = settingsOwner ?: return@action
        val text = withContext(Dispatchers.IO) { current.copyVisualizerPreset() }
        copy(text)
    }
    fun searchLyrics(token: CurrentMediaId, artist: String, title: String) = action {
        val query = LyricsQuery(artist.trim(), title.trim())
        lyricsSearch = token to query
        lyricsResults = null
        lyricsSearching = true
        lyricsOwner?.search(token, query)
    }
    fun previewLyrics(token: CurrentMediaId, result: LyricsSearchResult) = action { lyricsOwner?.preview(token, result) }
    fun saveLyricsResult(token: CurrentMediaId, result: LyricsSearchResult) = action { lyricsOwner?.saveResultToSource(token, result) }
    fun editLyrics(token: CurrentMediaId, text: String) = action { lyricsOwner?.updateLyricsText(token, text) }
    fun saveLyrics(token: CurrentMediaId) = action { lyricsOwner?.saveCurrentToSource(token, lyricsOffset) }
    fun clearLyrics(token: CurrentMediaId) = action {
        if (currentId != token) return@action
        lyricsOwner?.clearFetched(token)
        changeSettings { it.suppressFetchedLyrics(token.mediaUri) }
        bridge?.let { refreshLyrics(it) }
    }
    fun prepareDictionary() = action { lyricsOwner?.japaneseDictionary(true) }
    suspend fun lyricsText(token: CurrentMediaId, offset: Long = lyricsOffset): String = withContext(readings) {
        check(currentId == token) { translate("This isn't available") }
        sourceLyrics?.takeIf { it.mediaId() == token }?.text(offset) ?: error(translate("No lyrics available."))
    }
    fun exportLyrics(uri: Uri, token: CurrentMediaId, offset: Long) = action {
        val text = lyricsText(token, offset)
        withContext(Dispatchers.IO) {
            val output = connection.getApplication<Application>().contentResolver.openOutputStream(uri, "wt")
                ?: error(translate("Save Lyrics"))
            output.bufferedWriter().use { it.write(text) }
        }
        showFeedback(translate("Saved"))
    }
    suspend fun drawing(targets: List<Double>, width: Float, height: Float, frameNanos: Long, accent: AndroidRgb): AndroidVisualizerDrawing? {
        val player = bridge ?: return null
        return withContext(Dispatchers.Default) {
            player.visualizerDrawing(targets, width.toDouble(), height.toDouble(), frameNanos.toULong(), accent)
        }
    }

    fun togglePlayback() = action { service?.let { if (playback?.transport?.desiredPlaying == true) it.pause() else it.play() } }
    fun next() {
        if (connection.debugLogging) Log.d("Rufin", "Player next clicked run=${currentId?.run}")
        action { service?.next() }
    }
    fun previous() = action { service?.previous() }
    fun seek(millis: ULong) = action { service?.seekMillis(millis) }
    fun seekFromLyrics(token: CurrentMediaId, millis: ULong) = action {
        if (token == currentId) service?.seekMillis(playbackPositionForLyricsPosition(millis, lyricsOffset))
    }
    fun favorite() {
        val current = playback ?: return
        val uri = current.transport.current?.id?.mediaUri ?: return
        val effective = connection.projectedFavorite("track", uri, favorite)
        val favorite = !effective
        if (connection.debugLogging) Log.d("Rufin", "Player favorite clicked run=${currentId?.run} effective=$effective requested=$favorite")
        connection.setFavoriteIntent("track", uri, favorite)
        this.favorite = favorite
        action {
            try {
                val result = sourceOwner?.setFavoriteWithResult(FavoriteTarget.Track(uri), favorite)
                if (result != null && currentId?.mediaUri == uri && projectedFavorite(uri, favorite) == favorite)
                    this.favorite = result
            }
            finally {
                connection.releaseFavoriteIntent("track", uri, favorite)
            }
        }
    }
    fun shuffle() = action {
        val enabled = playback?.controls?.shuffleEnabled != true
        val current = service ?: return@action
        withContext(Dispatchers.IO) { current.setShuffle(enabled) }
    }
    fun autoDj() = action {
        val current = playbackOwner ?: return@action
        withContext(Dispatchers.IO) { current.toggleAutoDj() }
    }
    fun repeat() = action {
        val current = playbackOwner ?: return@action
        withContext(Dispatchers.IO) { current.cycleRepeat() }
    }
    suspend fun queuePage(offset: ULong, search: String = ""): PreparedQueuePage? = withContext(Dispatchers.Default) {
        playbackOwner?.queuePage(offset, 80u, search)
    }
    suspend fun createQueuePlaylist(name: String, sourceId: String?, public: Boolean?) {
        val source = sourceOwner ?: return
        val selection = playbackOwner?.selection() ?: return
        try { createPlaylist(source, sourceId, name, selection, public, sourceId != null)?.settingsError?.let { error(it) } }
        finally { selection.destroy() }
        connection.showFeedback(translate("Saved"))
    }
    suspend fun queueFavorite(mediaUri: String, favorite: Boolean): Boolean? {
        if (connection.debugLogging) Log.d("Rufin", "Queue favorite clicked run=${currentId?.run} current=${currentId?.mediaUri == mediaUri} requested=$favorite")
        connection.setFavoriteIntent("track", mediaUri, favorite)
        return try { sourceOwner?.setFavoriteWithResult(FavoriteTarget.Track(mediaUri), favorite) }
        finally {
            connection.releaseFavoriteIntent("track", mediaUri, favorite)
        }
    }
    internal fun projectedFavorite(uri: String, fallback: Boolean) = connection.projectedFavorite("track", uri, fallback)
    fun activateQueue(position: ULong) = action { playbackOwner?.activate(position) }
    fun removeQueue(position: ULong) = action { playbackOwner?.remove(position) }
    suspend fun moveQueue(position: ULong, target: ULong?, after: Boolean = false) {
        val current = playbackOwner ?: return
        val destination = target?.let { if (after) QueueReorderTarget.After(it) else QueueReorderTarget.Before(it) } ?: QueueReorderTarget.End
        withContext(Dispatchers.IO) { current.reorder(QueueSelection.Positions(listOf(position)), destination) }
    }
    fun clearQueue(includeCurrent: Boolean) = action { playbackOwner?.clear(includeCurrent) }
    fun visualizer(enabled: Boolean) = action { bridge?.setVisualizerEnabled(enabled) }
    suspend fun outputs(): List<RemoteOutput> {
        outputChoices = emptyList()
        val choices = playbackOwner?.remoteOutputs().orEmpty()
        outputChoices = choices
        return choices
    }
    suspend fun selectOutput(output: PlaybackOutput) { playbackOwner?.chooseOutput(output) }
    suspend fun selectAndroidOutput(routeId: String) {
        playbackOwner?.chooseOutput(PlaybackOutput.Local)
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
