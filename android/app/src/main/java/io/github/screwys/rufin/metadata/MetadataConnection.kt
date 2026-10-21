package io.github.screwys.rufin.metadata

import android.content.Context
import android.net.Uri
import java.io.ByteArrayOutputStream
import androidx.compose.runtime.*
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.MetadataEditor
import io.github.screwys.rufin.shared.rufin_core.MetadataItemId
import io.github.screwys.rufin.shared.rufin_core.MetadataEditorSnapshot
import io.github.screwys.rufin.shared.rufin_core.MetadataSaveSummary
import io.github.screwys.rufin.shared.rufin_core.MetadataLookupReview
import io.github.screwys.rufin.shared.rufin_core.MetadataLookupItem
import io.github.screwys.rufin.shared.rufin_core.MetadataLookupProgress
import io.github.screwys.rufin.shared.sources.ArtworkStorage
import io.github.screwys.rufin.shared.metadata_lookup.ArtworkResult
import io.github.screwys.rufin.shared.metadata_lookup.MetadataCandidate
import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.decodePlayerArtwork
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class MetadataConnection(
    private val model: RufinConnection, context: Context,
    private val kind: String, private val mediaUri: String,
    private val scope: CoroutineScope, private val onSaved: () -> Unit,
) {
    private val resolver = context.applicationContext.contentResolver
    var bridge by mutableStateOf<MetadataEditor?>(null)
        private set
    var state by mutableStateOf<MetadataEditorSnapshot?>(null)
        private set
    var busy by mutableStateOf(false)
        private set
    var failure by mutableStateOf<Throwable?>(null)
        private set
    var artwork by mutableStateOf<PlayerArtwork?>(null)
        private set
    var loadingArtwork by mutableStateOf(false)
        private set
    var results by mutableStateOf<List<ArtworkResult>?>(null)
        private set
    var searching by mutableStateOf(false)
        private set
    var saveConfirmation by mutableStateOf<MetadataSaveSummary?>(null)
        private set
    var lookupReview by mutableStateOf<MetadataLookupReview?>(null)
        private set
    var lookupItem by mutableStateOf<MetadataLookupItem?>(null)
        private set
    var lookupProgress by mutableStateOf<MetadataLookupProgress?>(null)
        private set
    var lookupFailure by mutableStateOf<Throwable?>(null)
        private set
    var lookupBusy by mutableStateOf(false)
        private set
    var lookupAccepting by mutableStateOf(false)
        private set
    var lookupFillMissing by mutableStateOf(false)
        private set
    private var lookupRun: Job? = null
    private var lookupUpdates: Job? = null
    private var lookupAction: Job? = null
    private var artworkRevision: ULong? = null
    private var artworkTask: Job? = null
    private var searchTask: Job? = null

    suspend fun open() {
        busy = true
        failure = null
        try {
            artworkTask?.cancelAndJoin()
            searchTask?.cancelAndJoin()
            bridge?.destroy()
            bridge = null
            state = null
            artwork = null
            results = null
            val source = snapshotFlow { model.sourceOwner }.filterNotNull().first()
            val item = when (kind) {
                "track" -> MetadataItemId.Track(mediaUri)
                "album", "album_header" -> MetadataItemId.Album(mediaUri)
                "artist" -> MetadataItemId.Artist(mediaUri)
                else -> error("Choose a track, album or artist")
            }
            val editor = MetadataEditor.open(source, item)
            bridge = editor
            artworkRevision = null
            refresh(editor)
            artworkTask = scope.launch {
                loadingArtwork = true
                try { editor.loadCurrentArtwork()?.destroy(); refresh(editor) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { failure = error }
                finally { loadingArtwork = false }
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error }
        finally { busy = false }
    }

    private suspend fun refresh(editor: MetadataEditor) {
        state = editor.snapshot()
        if (artworkRevision != state?.artworkRevision) {
            artworkRevision = state?.artworkRevision
            artwork = editor.artwork()?.use { decodePlayerArtwork(it.bytes(), 512) }
        }
    }

    private fun change(action: (MetadataEditor) -> Unit) {
        val editor = bridge ?: return
        try { action(editor); state = editor.snapshot(); failure = null }
        catch (error: Exception) { failure = error }
    }

    fun setField(id: String, value: String) = change { it.setField(id, value) }
    fun setLocked(value: Boolean) = change { it.setLocked(value) }
    fun undo(id: String) = action { it.resetField(id) }
    fun setStorage(value: ArtworkStorage) = change { it.setArtworkStorage(value) }

    private fun action(block: suspend (MetadataEditor) -> Unit) {
        val editor = bridge ?: return
        scope.launch {
            busy = true
            failure = null
            try { block(editor); refresh(editor) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error; state = editor.snapshot() }
            finally { busy = false }
        }
    }

    fun openLookup() = action { editor ->
        artworkTask?.cancelAndJoin()
        val review = MetadataLookupReview.open(editor)
        lookupReview = review
        lookupFillMissing = false
        lookupFailure = null
        lookupProgress = review.progress()
        lookupItem = review.item(0UL)
        lookupRun = scope.launch { review.run() }
        lookupUpdates = scope.launch {
            while (true) {
                val progress = review.nextUpdate() ?: break
                lookupProgress = progress
                if (!lookupBusy) lookupItem = review.item(0UL)
                if (progress.finished) break
            }
        }
    }

    private fun lookupAction(block: suspend (MetadataLookupReview) -> Unit) {
        val review = lookupReview ?: return
        val previous = lookupAction
        lookupAction = scope.launch {
            previous?.cancelAndJoin()
            lookupBusy = true
            lookupFailure = null
            try { block(review); lookupItem = review.item(0UL) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { lookupFailure = error }
            finally { lookupBusy = false }
        }
    }

    fun chooseLookup(candidate: MetadataCandidate?) = lookupAction { it.choose(0UL, candidate) }
    fun searchLookup(query: String, delayed: Boolean = false) {
        if (lookupAccepting) return
        val text = query.trim()
        lookupAction {
            if (text.isNotEmpty()) {
                if (delayed) delay(600)
                it.search(0UL, text)
            }
        }
    }
    fun updateLookupFillMissing(value: Boolean) {
        lookupFillMissing = value
    }
    fun refreshLookup(query: String) = lookupAction { it.refresh(0UL, query.trim()) }
    fun stopLookup() { lookupReview?.cancel() }
    fun cancelLookup() {
        lookupReview?.cancel()
        lookupRun?.cancel(); lookupUpdates?.cancel(); lookupAction?.cancel()
        lookupReview?.destroy(); lookupReview = null; lookupItem = null; lookupProgress = null
        lookupBusy = false; lookupAccepting = false
    }
    fun acceptLookup() {
        val review = lookupReview ?: return
        lookupAction = scope.launch {
            lookupBusy = true; lookupAccepting = true
            try {
                review.accept(lookupFillMissing)
                bridge?.let { refresh(it) }
                cancelLookup()
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { lookupFailure = error }
            finally { lookupBusy = false; lookupAccepting = false }
        }
    }
    fun save() = action {
        if (kind == "track") {
            val summary = it.prepareSave()
            if (summary.tracks > 0UL) saveConfirmation = summary
        } else { it.save(); onSaved() }
    }
    fun cancelSave() { saveConfirmation = null }
    fun confirmSave() {
        saveConfirmation = null
        action { it.save(); onSaved() }
    }
    fun removeArtwork() = action { it.removeArtwork() }
    fun useCurrentArtwork() = action { it.useCurrentArtwork() }
    fun chooseArtwork(url: String) = action { editor -> editor.downloadArtwork(url).use { editor.selectArtwork(it) } }
    fun importArtwork(uri: Uri) = action { editor ->
        val limit = state?.maxArtworkBytes?.toInt() ?: return@action
        val bytes = withContext(Dispatchers.IO) {
            resolver.openInputStream(uri)?.use { input ->
                val output = ByteArrayOutputStream()
                val buffer = ByteArray(8192)
                var remaining = limit + 1
                while (remaining > 0) {
                    val count = input.read(buffer, 0, minOf(buffer.size, remaining))
                    if (count < 0) break
                    output.write(buffer, 0, count)
                    remaining -= count
                }
                output.toByteArray()
            } ?: error(translate("Could not read artwork"))
        }
        editor.prepareArtwork(bytes).use { editor.selectArtwork(it) }
    }

    fun search(artist: String, album: String) {
        val editor = bridge ?: return
        searchTask = scope.launch {
            searching = true
            failure = null
            try { results = editor.searchArtwork(artist, album) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error }
            finally { searching = false }
        }
    }

    fun reload() { scope.launch { open() } }
    fun close() { cancelLookup(); artworkTask?.cancel(); searchTask?.cancel(); bridge?.destroy(); bridge = null }
}
