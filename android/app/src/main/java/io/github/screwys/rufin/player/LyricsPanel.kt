package io.github.screwys.rufin.player

import android.graphics.Typeface
import android.os.SystemClock
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.gestures.stopScroll
import androidx.compose.animation.core.tween
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.withContext
import io.github.screwys.rufin.shared.lyrics.UserPreferences
import io.github.screwys.rufin.shared.lyrics.LyricsAgentRole
import io.github.screwys.rufin.shared.lyrics.externalLyricsProviderTitle
import io.github.screwys.rufin.shared.lyrics.shiftLrcTextTimestamps
import io.github.screwys.rufin.shared.playback.CurrentMediaId
import io.github.screwys.rufin.shared.playback.TransportStatus
import kotlinx.coroutines.CancellationException
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.settings.settingsTextFieldColors
import kotlin.math.ceil

@Composable
internal fun LyricsPanel(player: PlayerConnection, state: LazyListState) {
    LaunchedEffect(player.currentId, player.bridge, player.lyrics?.mediaId) { player.requestLyrics() }
    val current = player.currentLyrics
    val document = current?.document
    val preferences = player.settings?.lyrics
    Box(Modifier.fillMaxSize()) {
        when {
            current?.loading == true -> CircularProgressIndicator(Modifier.align(Alignment.Center))
            current?.instrumental == true -> Text(translate("Instrumental"), Modifier.align(Alignment.Center))
            document == null -> Text(translate("No lyrics available."), Modifier.align(Alignment.Center))
            else -> LyricsDocumentView(player, current, document, preferences, state)
        }
    }
}

@Composable
internal fun LyricsActions(player: PlayerConnection) {
    val current = player.currentLyrics
    val document = current?.document
    val preferences = player.settings?.lyrics
    val token = player.lyricsToken
    var search by remember { mutableStateOf(false) }
    var edit by remember { mutableStateOf(false) }
    var editOffset by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var exportToken by rememberSaveable(stateSaver = listSaver<CurrentMediaId?, String>(
        save = { media -> media?.let { listOf(it.mediaUri, it.run?.toString().orEmpty()) }.orEmpty() },
        restore = { values -> values.takeIf { it.size == 2 }?.let { CurrentMediaId(it[1].toULongOrNull(), it[0]) } },
    )) { mutableStateOf<CurrentMediaId?>(null) }
    var exportOffset by rememberSaveable { mutableLongStateOf(0L) }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        if (uri != null) exportToken?.let { player.exportLyrics(uri, it, exportOffset) }
        exportToken = null
    }
    Box {
        PlayerIconButton("rufin-sliders-2-symbolic", "Customize display", { menu = true })
        DropdownMenu(menu, { menu = false }) {
            DropdownMenuItem(text = { Text(translate("Search lyrics")) }, onClick = { menu = false; search = true },
                enabled = token != null && player.settings?.application?.privateMode != true,
                leadingIcon = { RufinIcon("rufin-lyrics-search-symbolic", null) })
            DropdownMenuItem(text = { Text(translate("Edit Lyrics")) }, onClick = { menu = false; edit = true },
                enabled = document != null && current?.writable == true,
                leadingIcon = { RufinIcon("rufin-document-edit-symbolic", null) })
            if (document != null && current != null) {
                DropdownMenuItem(text = { Text(translate("Save Lyrics")) }, onClick = {
                    menu = false
                    val captured = token ?: return@DropdownMenuItem
                    if (preferences?.saveLyricsToSource == true && current.writable) player.saveLyrics(captured)
                    else {
                        exportToken = captured
                        exportOffset = player.lyricsOffset
                        export.launch("${player.currentItem?.metadata?.title.orEmpty()}.lrc")
                    }
                }, leadingIcon = { RufinIcon("rufin-download-symbolic", null) })
                DropdownMenuItem(text = { Text(translate("Lyrics offset (ms)") + " · " + player.lyricsOffset) },
                    onClick = { menu = false; editOffset = true }, leadingIcon = { RufinIcon("rufin-preferences-system-time-symbolic", null) })
                DropdownMenuItem(text = { Text(translate("Clear fetched lyrics for this track")) },
                    onClick = { menu = false; token?.let(player::clearLyrics) }, enabled = current.clearable,
                    leadingIcon = { RufinIcon("rufin-process-stop-symbolic", null) })
            }
        }
    }
    if (search && token != null) LyricsSearchSheet(player, token) { search = false }
    if (edit && token != null) LyricsEditSheet(player, token) { edit = false }
    if (editOffset) LyricsOffsetDialog(player) { editOffset = false }
}

@Composable
internal fun CurrentLyricsPreview(player: PlayerConnection) {
    LaunchedEffect(player.currentId, player.bridge, player.lyrics?.mediaId) { player.requestLyrics() }
    val current = player.currentLyrics
    val document = current?.document
    val snapshot = current?.snapshot
    val preferences = player.settings?.lyrics
    val karaoke = preferences?.karaokeMode == true
    val reduceMotion = LocalReduceMotion.current
    val frame by rememberLyricsFrame(player, snapshot, karaoke)
    val active = frame?.activeLine?.toInt()
    val highlights = frame?.highlights.orEmpty()
    val normal = MaterialTheme.colorScheme.onSurface
    val selected = preferences?.lyricsHighlightColor?.let { playerLyricsColor(it) }
        ?: MaterialTheme.colorScheme.primary
    val shadow = Shadow(MaterialTheme.colorScheme.background, Offset.Zero, 4f)
    val fontSize = MaterialTheme.typography.bodyLarge.fontSize.value.toInt()
    AnimatedContent(active, Modifier.fillMaxWidth(), transitionSpec = {
        (slideInVertically(tween(if (reduceMotion) 0 else 180)) { it / 2 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
            (slideOutVertically(tween(if (reduceMotion) 0 else 180)) { -it / 2 } + fadeOut(tween(if (reduceMotion) 0 else 120)))
    }, label = "currentLyricsLine") { index ->
        val line = index?.let { document?.lines?.getOrNull(it) }
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
            line?.let {
                if (it.cueLines.isNotEmpty()) it.cueLines.forEachIndexed { cueIndex, cueLine ->
                    val color = if (document?.agents?.firstOrNull { voice -> voice.id == cueLine.agentId }?.role == LyricsAgentRole.BACKGROUND)
                        MaterialTheme.colorScheme.secondary else selected
                    val cueHighlights = index?.let { highlights[it.toUInt()] }?.getOrNull(cueIndex)
                    LyricsText(cueLine.text, cueLine.reading, preferences,
                        FontFamily.Default, fontSize, normal, false, shadow, cueHighlights, color, cueRanges(cueLine), centered = false, maxLines = 1)
                    if (karaoke && preferences?.showRomanization == true) {
                        cueLine.reading?.takeIf { reading -> reading.romanization.isNotBlank() }?.let { reading ->
                            HighlightedLyricsText(reading.romanization, romanizationRanges(reading), cueHighlights?.romanization.orEmpty(), color, maxLines = 1,
                                fontSize = 12.sp, color = normal, style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                        }
                    }
                } else LyricsText(it.text, it.reading, preferences, FontFamily.Default, fontSize, normal,
                    false, shadow, centered = false, maxLines = 1)
                if (preferences?.showRomanization == true &&
                    (!karaoke || it.cueLines.none { cue -> cue.reading?.romanization?.isNotBlank() == true })) {
                    val romanization = it.pronunciation ?: it.reading?.romanization
                    if (!romanization.isNullOrBlank()) Text(romanization, maxLines = 1, fontSize = 12.sp, color = normal,
                        style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                }
            }
        }
    }
}

@Composable
private fun rememberLyricsFrame(player: PlayerConnection, snapshot: AndroidLyricsSnapshot?, karaoke: Boolean,
    state: LazyListState? = null,
): State<AndroidLyricsFrame?> {
    val frame = remember(snapshot) { mutableStateOf<AndroidLyricsFrame?>(null) }
    val offset = player.lyricsOffset
    LaunchedEffect(snapshot, karaoke, offset, state) {
        if (snapshot == null) return@LaunchedEffect
        snapshotFlow {
            val lines = state?.layoutInfo?.visibleItemsInfo?.let { visible ->
                val first = visible.firstOrNull()?.index ?: 0
                first..(visible.lastOrNull()?.index ?: first)
            }
            player.playback?.transport to lines
        }.collectLatest { (transport, lines) ->
            if (transport == null) return@collectLatest
            while (true) {
                val position = player.positionNow()
                val started = SystemClock.elapsedRealtime()
                val next = withContext(Dispatchers.Default) {
                    snapshot.frame(position, offset, karaoke, currentLineOnly = lines == null,
                        firstLine = (lines?.first ?: 0).toUInt(), lastLine = (lines?.last ?: 0).toUInt())
                }
                frame.value = next
                if (transport.state != TransportStatus.PLAYING) break
                val updateAfter = next.updateAfterMillis ?: break
                val remaining = ceil(updateAfter.toDouble() / transport.playbackRate).toLong() -
                    (SystemClock.elapsedRealtime() - started)
                delay(remaining.coerceAtLeast(1L))
            }
        }
    }
    return frame
}

@Composable
private fun LyricsOffsetDialog(player: PlayerConnection, onDismiss: () -> Unit) {
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate("Lyrics offset (ms)")) }, text = {
        LyricsOffsetControls(player.lyricsOffset) { player.lyricsOffset = it }
    }, confirmButton = { TextButton(onDismiss) { Text(translate("Close")) } })
}

@Composable
private fun LyricsOffsetControls(value: Long, enabled: Boolean = true, onApply: (Long) -> Unit) {
    var text by remember(value) { mutableStateOf(value.toString()) }
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val fieldWidth = (maxWidth - 200.dp).coerceAtLeast(160.dp)
        Row(Modifier.horizontalScroll(rememberScrollState()), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            listOf(-100L, -50L).forEach { delta ->
                TextButton({ onApply((text.toLongOrNull() ?: value) + delta) }, Modifier.widthIn(min = 48.dp),
                    contentPadding = PaddingValues(horizontal = 2.dp), enabled = enabled) { Text(delta.toString(), maxLines = 1) }
            }
            OutlinedTextField(text, { input -> text = input; input.toLongOrNull()?.let(onApply) },
                colors = settingsTextFieldColors(),
                singleLine = true, enabled = enabled,
                label = { Text(translate("Lyrics offset (ms)"), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                suffix = { Text("ms") },
                trailingIcon = { PlayerIconButton("rufin-view-refresh-symbolic", "Reset", { text = "0"; onApply(0L) }, enabled = enabled) },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.width(fieldWidth))
            listOf(50L, 100L).forEach { delta ->
                TextButton({ onApply((text.toLongOrNull() ?: value) + delta) }, Modifier.widthIn(min = 48.dp),
                    contentPadding = PaddingValues(horizontal = 2.dp), enabled = enabled) { Text("+$delta", maxLines = 1) }
            }
        }
    }
}

@Composable
private fun LyricsDocumentView(player: PlayerConnection, current: AndroidLyricsState, document: AndroidLyricsDocument,
    preferences: UserPreferences?, state: LazyListState,
) {
    val snapshot = current.snapshot!!
    val scope = rememberCoroutineScope()
    val karaoke = preferences?.karaokeMode == true
    val timing by rememberLyricsFrame(player, snapshot, karaoke, state)
    val highlights = timing?.highlights.orEmpty()
    val dragged by state.interactionSource.collectIsDraggedAsState()
    var pausedUntil by remember { mutableLongStateOf(0L) }
    var hadDrag by remember { mutableStateOf(false) }
    var followRevision by remember { mutableIntStateOf(0) }
    val pauseMillis = player.preferenceOptions?.lyricsScrollPauseMillis?.toLong() ?: 0L
    val reduceMotion = LocalReduceMotion.current
    LaunchedEffect(dragged) {
        if (dragged) hadDrag = true
        else if (hadDrag) {
            pausedUntil = SystemClock.elapsedRealtime() + pauseMillis
            delay(pauseMillis)
            followRevision++
        }
    }
    LaunchedEffect(current.mediaId) { hadDrag = false; pausedUntil = 0 }
    val active = timing?.activeLine?.toInt()
    val scroll = timing?.scrollLine?.toInt()
    LaunchedEffect(scroll, followRevision) {
        if (scroll != null && !dragged && SystemClock.elapsedRealtime() >= pausedUntil) {
            if (state.layoutInfo.visibleItemsInfo.none { it.index == scroll }) {
                val center = (state.layoutInfo.viewportStartOffset + state.layoutInfo.viewportEndOffset) / 2
                if (reduceMotion) state.scrollToItem(scroll, -center)
                else state.animateScrollToItem(scroll, -center)
            }
            state.layoutInfo.visibleItemsInfo.firstOrNull { it.index == scroll }?.let { line ->
                val center = (state.layoutInfo.viewportStartOffset + state.layoutInfo.viewportEndOffset) / 2f
                val distance = line.offset + line.size / 2f - center
                if (reduceMotion) state.scrollBy(distance) else state.animateScrollBy(distance, tween(180))
            }
        }
    }
    val fontName = preferences?.fullscreenLyricsFontFamily
    val family = remember(fontName) { fontName?.let { FontFamily(Typeface.create(it, Typeface.NORMAL)) } ?: FontFamily.Default }
    val fontSize = preferences?.fullscreenLyricsFontSize?.toInt() ?: 19
    val fallbackColor = MaterialTheme.colorScheme.primary
    val selectedColor = preferences?.lyricsHighlightColor?.let(::playerLyricsColor) ?: fallbackColor
    val normal = MaterialTheme.colorScheme.onSurfaceVariant
    val shadow = Shadow(MaterialTheme.colorScheme.background, Offset.Zero, 4f)
    BoxWithConstraints(Modifier.fillMaxSize()) {
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(horizontal = 24.dp, vertical = maxHeight / 2),
        verticalArrangement = Arrangement.spacedBy(18.dp)) {
        items(document.lines.size) { index ->
            val line = document.lines[index]
            val lineActive = index == active || timing?.highlightAll == true
            val color = MaterialTheme.colorScheme.onSurface
            val opacity by animateFloatAsState(if (lineActive) 1f else .34f,
                tween(if (reduceMotion) 0 else 180), label = "lyricsOpacity")
            val scale by animateFloatAsState(if (lineActive) 1.04f else .96f,
                tween(if (reduceMotion) 0 else 180), label = "lyricsScale")
            Column(Modifier.fillMaxWidth().graphicsLayer {
                alpha = opacity
                scaleX = scale
                scaleY = scale
            }.then(if (line.startMillis == null) Modifier else Modifier.clickable {
                hadDrag = false
                pausedUntil = 0L
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) {
                    state.stopScroll()
                    if (index == scroll) followRevision++
                    current.currentMediaId?.let { player.seekFromLyrics(it, line.startMillis!!) }
                }
            }), horizontalAlignment = Alignment.CenterHorizontally) {
                if (line.cueLines.isNotEmpty()) line.cueLines.forEachIndexed { cueIndex, cueLine ->
                    val voice = document.agents.firstOrNull { it.id == cueLine.agentId }
                    val voiceColor = if (voice?.role == LyricsAgentRole.BACKGROUND) MaterialTheme.colorScheme.secondary else selectedColor
                    voice?.name?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = normal) }
                    val cueHighlights = highlights[index.toUInt()]?.getOrNull(cueIndex)
                    LyricsText(cueLine.text, cueLine.reading, preferences, family, fontSize, color, lineActive, shadow, cueHighlights, voiceColor, cueRanges(cueLine))
                    if (karaoke && preferences?.showRomanization == true) {
                        cueLine.reading?.takeIf { it.romanization.isNotBlank() }?.let { reading ->
                            HighlightedLyricsText(reading.romanization, romanizationRanges(reading), cueHighlights?.romanization.orEmpty(), voiceColor, Modifier.fillMaxWidth().padding(top = 6.dp),
                                fontFamily = family, fontSize = (fontSize * .7f).sp, color = color, textAlign = TextAlign.Center,
                                style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                        }
                    }
                } else LyricsText(line.text, line.reading, preferences, family, fontSize, color, lineActive, shadow)
                if (preferences?.showRomanization == true &&
                    (!karaoke || line.cueLines.none { it.reading?.romanization?.isNotBlank() == true })) {
                    val romanization = line.pronunciation ?: line.reading?.romanization
                    if (!romanization.isNullOrBlank()) Text(romanization, Modifier.padding(top = 6.dp), fontFamily = family,
                        fontSize = (fontSize * .7f).sp, color = color, textAlign = TextAlign.Center,
                        style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                }
            }
        }
    }
    }
}

private data class LyricsTextRange(val start: Int, val end: Int, val index: Int)

@Composable
private fun cueRanges(line: AndroidLyricsCueLine): List<LyricsTextRange> = remember(line) {
    line.cues.mapIndexedNotNull { index, cue ->
        val start = cue.utf16Start?.toInt() ?: return@mapIndexedNotNull null
        val end = cue.utf16End?.toInt() ?: return@mapIndexedNotNull null
        LyricsTextRange(start, end, index)
    }
}

@Composable
private fun romanizationRanges(reading: AndroidJapaneseReading): List<LyricsTextRange> = remember(reading) {
    reading.romanizationSpans.mapIndexed { index, span -> LyricsTextRange(span.start.toInt(), span.end.toInt(), index) }
}

@Composable
private fun HighlightedLyricsText(text: String, ranges: List<LyricsTextRange>, progress: List<Double>, highlightColor: Color,
    modifier: Modifier = Modifier, color: Color, style: TextStyle, fontFamily: FontFamily? = null,
    fontSize: TextUnit, fontWeight: FontWeight? = null, textAlign: TextAlign? = null, maxLines: Int = Int.MAX_VALUE,
) {
    var layout by remember(text) { mutableStateOf<TextLayoutResult?>(null) }
    val paths = remember(layout, ranges) { layout?.let { result -> ranges.map { result.getPathForRange(it.start, it.end) } }.orEmpty() }
    Text(text, modifier.drawWithContent {
        drawContent()
        layout?.let { result ->
            ranges.forEachIndexed { index, range ->
                val amount = progress.getOrNull(range.index) ?: return@forEachIndexed
                if (amount > 0.0) paths.getOrNull(index)?.let { path ->
                    clipPath(path) { drawText(result, color = lerp(color, highlightColor, amount.toFloat())) }
                }
            }
        }
    }, color = color, style = style, fontFamily = fontFamily, fontSize = fontSize,
        fontWeight = fontWeight, textAlign = textAlign, maxLines = maxLines, onTextLayout = { layout = it })
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LyricsText(text: String, reading: AndroidJapaneseReading?, preferences: UserPreferences?,
    family: FontFamily, size: Int, color: Color, active: Boolean, shadow: Shadow,
    highlights: AndroidLyricsHighlight? = null, highlightColor: Color = color,
    ranges: List<LyricsTextRange> = emptyList(), centered: Boolean = true, maxLines: Int = Int.MAX_VALUE,
) {
    val style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow)
    val weight = if (active) FontWeight.ExtraBold else FontWeight.SemiBold
    if (preferences?.showFurigana == true && reading?.segments?.isNotEmpty() == true) {
        val segmentRanges = remember(reading, ranges) {
            var cursor = 0
            reading.segments.map { segment ->
                val start = cursor
                cursor += segment.surface.length
                ranges.mapNotNull { range ->
                    val from = maxOf(start, range.start)
                    val to = minOf(cursor, range.end)
                    if (from < to) LyricsTextRange(from - start, to - start, range.index) else null
                }
            }
        }
        FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(1.dp,
            if (centered) Alignment.CenterHorizontally else Alignment.Start), maxLines = maxLines) {
            reading.segments.forEachIndexed { index, segment ->
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    val progress = highlights?.furigana?.getOrNull(index)
                    Text(segment.furigana.orEmpty(), fontFamily = family, fontSize = (size * .5f).sp,
                        color = progress?.let { lerp(color, highlightColor, it.toFloat()) } ?: color, style = style)
                    HighlightedLyricsText(segment.surface, segmentRanges[index], highlights?.cues.orEmpty(), highlightColor,
                        fontFamily = family, fontSize = size.sp, color = color, style = style, fontWeight = weight)
                }
            }
        }
    } else HighlightedLyricsText(text, ranges, highlights?.cues.orEmpty(), highlightColor,
        Modifier.fillMaxWidth(), fontFamily = family, fontSize = size.sp, color = color,
        textAlign = if (centered) TextAlign.Center else TextAlign.Start, maxLines = maxLines,
        style = style, fontWeight = weight)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LyricsSearchSheet(player: PlayerConnection, token: CurrentMediaId, onDismiss: () -> Unit) {
    var artist by rememberSaveable(token) { mutableStateOf(player.currentItem?.metadata?.artist.orEmpty()) }
    var title by rememberSaveable(token) { mutableStateOf(player.currentItem?.metadata?.title.orEmpty()) }
    val matchingSearch = player.lyricsSearch?.let { it.first == token && it.second.artistName == artist.trim() && it.second.trackName == title.trim() } == true
    val results = if (matchingSearch) player.lyricsResults else null
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().imePadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate("Search Lyrics"), style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(artist, { artist = it }, label = { Text(translate("Artist")) }, singleLine = true, modifier = Modifier.fillMaxWidth(), colors = settingsTextFieldColors())
            OutlinedTextField(title, { title = it }, label = { Text(translate("Track")) }, singleLine = true, modifier = Modifier.fillMaxWidth(), colors = settingsTextFieldColors())
            Button({ player.searchLyrics(token, artist, title) }, enabled = player.lyricsToken == token && player.settings?.application?.privateMode != true) { Text(translate("Search")) }
            if (matchingSearch && player.lyricsSearching) CircularProgressIndicator()
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 360.dp)) {
                items(results.orEmpty(), key = { "${it.provider}:${it.id}" }) { hit ->
                    ListItem(headlineContent = { Text(hit.trackName) }, supportingContent = { Text("${hit.artistName} · ${hit.albumName}\n${externalLyricsProviderTitle(hit.provider)}") },
                        modifier = Modifier.clickable { player.previewLyrics(token, hit); onDismiss() })
                }
                if (matchingSearch && !player.lyricsSearching && results?.isEmpty() == true) item {
                    Text(translate("No results"), Modifier.padding(16.dp))
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LyricsEditSheet(player: PlayerConnection, token: CurrentMediaId, onDismiss: () -> Unit) {
    var text by rememberSaveable(token) { mutableStateOf("") }
    var appliedOffset by rememberSaveable(token) { mutableLongStateOf(player.lyricsOffset) }
    var loaded by rememberSaveable(token) { mutableStateOf(false) }
    LaunchedEffect(token) {
        if (loaded) return@LaunchedEffect
        try { text = player.lyricsText(token).orEmpty(); loaded = true }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.9f).imePadding().padding(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Edit Lyrics"), Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                TextButton({ player.editLyrics(token, text); onDismiss() },
                    enabled = loaded && player.lyricsToken == token && text.isNotBlank()) { Text(translate("Save")) }
            }
            if (!loaded) LinearProgressIndicator(Modifier.fillMaxWidth())
            LyricsOffsetControls(appliedOffset, loaded) { offset ->
                val delta = (offset.toBigInteger() - appliedOffset.toBigInteger()).coerceIn(Long.MIN_VALUE.toBigInteger(), Long.MAX_VALUE.toBigInteger()).toLong()
                val shifted = shiftLrcTextTimestamps(text, delta)
                text = shifted
                appliedOffset = offset
                player.lyricsOffset = offset
            }
            OutlinedTextField(text, { text = it }, modifier = Modifier.fillMaxWidth().weight(1f),
                colors = settingsTextFieldColors(),
                textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false), enabled = loaded)
        }
    }
}
