package io.github.screwys.rufin.player

import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.VisibilityThreshold
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.spring
import androidx.compose.ui.unit.IntSize
import androidx.compose.animation.expandHorizontally
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.shrinkHorizontally
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.text.TextAutoSize
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.layer.GraphicsLayer
import androidx.compose.ui.graphics.layer.drawLayer
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.rememberGraphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import androidx.core.content.FileProvider
import io.github.screwys.rufin.browse.OnBand
import io.github.screwys.rufin.browse.showcaseBand
import io.github.screwys.rufin.core.AndroidLyricsDocument
import io.github.screwys.rufin.core.AndroidLyricsLine
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.CoverShape
import io.github.screwys.rufin.ui.PlayerArtwork
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SegmentedPill
import io.github.screwys.rufin.ui.rememberArtwork
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import kotlin.math.absoluteValue

/** What a share card shows: the track, or lines of its lyrics. */
internal enum class ShareKind { SONG, LYRICS }

/** Where a card takes its colours: the track's cover, the app theme, or a fixed colour. */
internal enum class CardColours {
    DYNAMIC, THEME, STATIC;

    val title get() = when (this) { DYNAMIC -> translate("Dynamic"); THEME -> translate("Theme"); STATIC -> translate("Static") }
}

/** The cards' colours, kept while the sheet closes to pick lyrics lines. */
internal class ShareCardStyle {
    var colours by mutableStateOf(CardColours.DYNAMIC)
    var fixed by mutableStateOf(staticColours.first())
}

/** The fixed colours Static offers. */
private val staticColours = listOf(Color(0xFF3F6FD8), Color(0xFF2E9E62), Color(0xFFE0A100),
    Color(0xFFD9483B), Color(0xFF7E57C2), Color(0xFFD65DB1))

/** As Navic's: a card fits about this many characters before its text gets too small. */
private const val SHARE_CHARACTERS = 150

/** The selection after a tap on line `index`: lines stay one run, as on a card. A tap next to the run
 *  extends it, on its end shortens it, and anywhere else starts again there. */
internal fun lyricsSelectionAfterTap(lines: List<String>, selection: List<Int>, index: Int): List<Int> {
    val characters = { picked: List<Int> -> picked.sumOf { lines[it].length } }
    val next = when {
        selection.isEmpty() -> listOf(index)
        index == selection.first() || index == selection.last() -> selection - index
        index in selection -> listOf(index)
        index == selection.first() - 1 -> listOf(index) + selection
        index == selection.last() + 1 -> selection + index
        else -> listOf(index)
    }
    return if (characters(next) <= SHARE_CHARACTERS || next.size == 1) next else selection
}

/** The text of a lyrics line: its own, or its voices' lines one under the other. */
internal fun lyricsShareText(line: AndroidLyricsLine): String =
    line.text.ifBlank { line.cueLines.joinToString("\n") { it.text } }

/** The lines a lyrics card starts with: the playing line and those after it that fit the card. */
internal fun defaultShareLines(document: AndroidLyricsDocument, positionMillis: ULong): List<Int> {
    val texts = document.lines.map(::lyricsShareText)
    val playing = document.lines.indexOfLast { it.startMillis != null && it.startMillis!! <= positionMillis }.coerceAtLeast(0)
    val first = (playing until texts.size).firstOrNull { texts[it].isNotBlank() } ?: return emptyList()
    var picked = listOf(first)
    while (picked.size < 4) {
        val next = picked.last() + 1
        if (next >= texts.size || texts[next].isBlank()) break
        val longer = lyricsSelectionAfterTap(texts, picked, next)
        if (longer.size == picked.size) break
        picked = longer
    }
    return picked
}

/** The share sheet: a song card and, when the track has lyrics, a lyrics card, side by side. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlayerShareSheet(player: PlayerConnection, initial: ShareKind, style: ShareCardStyle, chosenLines: List<Int>?,
    onEditLyrics: (List<Int>) -> Unit, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    val duration = if (reduceMotion) 0 else 280
    LaunchedEffect(player.currentId, player.bridge) { player.requestLyrics() }
    val document = player.currentLyrics?.document?.takeIf { it.lines.isNotEmpty() }
    val selection = remember(document, chosenLines) { document?.let { chosenLines ?: defaultShareLines(it, player.positionNow()) }.orEmpty() }
    val lines = selection.mapNotNull { document?.lines?.getOrNull(it)?.let(::lyricsShareText) }
    val kinds = if (lines.isEmpty()) listOf(ShareKind.SONG) else ShareKind.entries
    val pager = rememberPagerState(kinds.indexOf(initial).coerceAtLeast(0)) { kinds.size }
    val layers = listOf(rememberGraphicsLayer(), rememberGraphicsLayer())
    val artwork = rememberArtwork(player.currentItem?.artworkBinding, player, with(LocalDensity.current) { 320.dp.roundToPx() })
    val scheme = MaterialTheme.colorScheme
    val fill by animateColorAsState(when (style.colours) { CardColours.THEME -> scheme.primary; CardColours.STATIC -> style.fixed
        CardColours.DYNAMIC -> Color.Transparent }, tween(duration), label = "shareCardFill")
    val content by animateColorAsState(when (style.colours) {
        CardColours.DYNAMIC -> OnBand
        CardColours.THEME -> scheme.onPrimary
        CardColours.STATIC -> if (style.fixed.luminance() > .5f) Color.Black.copy(alpha = .87f) else Color.White
    }, tween(duration), label = "shareCardContent")
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally) {
            // The other card peeks in beside the one shown and grows as it comes in.
            HorizontalPager(pager, Modifier.fillMaxWidth().widthIn(max = 480.dp), contentPadding = PaddingValues(horizontal = 44.dp),
                pageSpacing = 12.dp, verticalAlignment = Alignment.CenterVertically) { page ->
                val away = ((pager.currentPage - page) + pager.currentPageOffsetFraction).absoluteValue.coerceIn(0f, 1f)
                ShareCard(layers[page], artwork, fill, content, kinds[page] == ShareKind.LYRICS, Modifier.graphicsLayer {
                    val scale = 1f - .08f * away
                    scaleX = scale
                    scaleY = scale
                }) {
                    if (kinds[page] == ShareKind.SONG) SongCardContent(player, artwork, content)
                    else LyricsCardContent(player, artwork, lines, content)
                }
            }
            Spacer(Modifier.height(20.dp))
            if (kinds.size > 1) SegmentedPill(kinds, kinds[pager.targetPage], { kind ->
                scope.launch { if (reduceMotion) pager.scrollToPage(kinds.indexOf(kind)) else pager.animateScrollToPage(kinds.indexOf(kind)) }
            }, Modifier.fillMaxWidth().padding(horizontal = 32.dp), equal = true) { kind, _ ->
                Text(translate(if (kind == ShareKind.SONG) "Song" else "Lyrics"), style = MaterialTheme.typography.titleSmall)
            }
            Spacer(Modifier.height(12.dp))
            ColourPill(style)
            AnimatedVisibility(kinds[pager.targetPage] == ShareKind.LYRICS,
                enter = expandVertically(tween(duration)) + fadeIn(tween(duration)),
                exit = shrinkVertically(tween(duration)) + fadeOut(tween(duration / 2))) {
                FilledTonalButton({ onEditLyrics(selection) }, Modifier.padding(top = 12.dp)) {
                    RufinIcon("rufin-document-edit-symbolic", null, Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(translate("Edit Lyrics"))
                }
            }
            Spacer(Modifier.height(20.dp))
            Button({
                scope.launch {
                    shareImage(context, layers[pager.currentPage], if (kinds[pager.currentPage] == ShareKind.SONG) "song.png" else "lyrics.png")
                    onDismiss()
                }
            }, Modifier.fillMaxWidth().padding(horizontal = 24.dp).height(52.dp)) {
                RufinIcon("rufin-share-symbolic", null, Modifier.size(20.dp))
                Spacer(Modifier.width(8.dp))
                Text(translate("Share"), style = MaterialTheme.typography.titleMedium)
            }
        }
    }
}

/** Dynamic, Theme and Static in one pill. Picked, Static opens its colours beside its name, and the pill
 *  scrolls so they show; Dynamic and Theme stay a scroll away. */
@Composable
private fun ColourPill(style: ShareCardStyle) {
    val reduceMotion = LocalReduceMotion.current
    val duration = if (reduceMotion) 0 else 280
    val grow = if (reduceMotion) snap() else spring(dampingRatio = .9f, stiffness = Spring.StiffnessMediumLow,
        visibilityThreshold = IntSize.VisibilityThreshold)
    val scroll = rememberScrollState()
    // While Static's colours open the pill stays scrolled to its end with them; leaving, it goes back.
    LaunchedEffect(style.colours) {
        if (style.colours == CardColours.STATIC) snapshotFlow { scroll.maxValue }.collect { scroll.scrollTo(it) }
        else scroll.animateScrollTo(0, if (reduceMotion) snap() else spring(dampingRatio = .9f, stiffness = Spring.StiffnessMediumLow))
    }
    SegmentedPill(CardColours.entries, style.colours, { style.colours = it }, Modifier.padding(horizontal = 24.dp), scroll = scroll) { option, picked ->
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(option.title, style = MaterialTheme.typography.titleSmall)
            if (option == CardColours.STATIC) AnimatedVisibility(picked,
                enter = expandHorizontally(grow, expandFrom = Alignment.Start) + fadeIn(tween(duration, delayMillis = duration / 4)),
                exit = shrinkHorizontally(grow, shrinkTowards = Alignment.Start) + fadeOut(tween(duration / 2))) {
                Row(Modifier.padding(start = 14.dp, end = 2.dp), horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                    staticColours.forEachIndexed { index, colour ->
                        val chosen = colour == style.fixed
                        Box(Modifier.size(26.dp)
                            .animateEnterExit(enter = scaleIn(if (reduceMotion) snap() else spring(dampingRatio = .6f,
                                stiffness = Spring.StiffnessMediumLow), initialScale = .4f) + fadeIn(tween(duration, delayMillis = index * 35)))
                            .clip(CircleShape).background(colour)
                            .border(if (chosen) BorderStroke(2.dp, MaterialTheme.colorScheme.inverseOnSurface) else BorderStroke(0.dp, Color.Transparent), CircleShape)
                            .semantics { selected = chosen }.clickable { style.fixed = colour })
                    }
                }
            }
        }
    }
}

/** A card in the chosen colours, drawn into `layer` for sharing. */
@Composable
private fun ShareCard(layer: GraphicsLayer, artwork: PlayerArtwork?, fill: Color, content: Color, portrait: Boolean, modifier: Modifier,
    body: @Composable ColumnScope.() -> Unit) {
    // Lyrics take a 4:5 card; a song card is as tall as its full-width cover and text.
    Box(modifier.fillMaxWidth().then(if (portrait) Modifier.aspectRatio(4f / 5f) else Modifier).drawWithContent {
        layer.record { this@drawWithContent.drawContent() }
        drawLayer(layer)
    }.clip(RoundedCornerShape(24.dp)).then(showcaseBand(artwork?.band)).background(fill)) {
        CompositionLocalProvider(LocalContentColor provides content) {
            Column(Modifier.fillMaxWidth().then(if (portrait) Modifier.fillMaxHeight() else Modifier).padding(24.dp), content = body)
        }
    }
}

@Composable
private fun ColumnScope.SongCardContent(player: PlayerConnection, artwork: PlayerArtwork?, content: Color) {
    val metadata = player.currentItem?.metadata
    Artwork(artwork, Modifier.fillMaxWidth().aspectRatio(1f).clip(CoverShape))
    Spacer(Modifier.height(16.dp))
    Text(metadata?.title.orEmpty(), style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold, maxLines = 2)
    Text(metadata?.artist.orEmpty(), style = MaterialTheme.typography.bodyLarge, maxLines = 1, color = content.copy(alpha = .8f))
    Spacer(Modifier.height(12.dp))
    ShareMark()
}

@Composable
private fun ColumnScope.LyricsCardContent(player: PlayerConnection, artwork: PlayerArtwork?, lines: List<String>, content: Color) {
    val metadata = player.currentItem?.metadata
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Artwork(artwork, Modifier.size(44.dp).clip(CoverShape))
        Column {
            Text(metadata?.title.orEmpty(), style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold, maxLines = 1)
            Text(metadata?.artist.orEmpty(), style = MaterialTheme.typography.bodySmall, maxLines = 1, color = content.copy(alpha = .8f))
        }
    }
    Box(Modifier.weight(1f).fillMaxWidth().padding(vertical = 16.dp), contentAlignment = Alignment.CenterStart) {
        BasicText(lines.joinToString("\n"), Modifier.fillMaxWidth(),
            style = TextStyle(color = content, fontWeight = FontWeight.Bold, lineHeight = 1.3.em),
            autoSize = TextAutoSize.StepBased(12.sp, 36.sp, 1.sp))
    }
    ShareMark()
}

@Composable
private fun ShareMark() {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        RufinIcon("rufin-cover-fallback-symbolic", null, Modifier.size(18.dp), LocalContentColor.current)
        Text("Rufin", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
    }
}

/** Saves the card as a PNG and opens Android's share sheet with it. */
private suspend fun shareImage(context: Context, layer: GraphicsLayer, name: String) {
    val uri = withContext(Dispatchers.IO) {
        val image = layer.toImageBitmap().asAndroidBitmap().let {
            // A hardware bitmap cannot be compressed; a plain copy can.
            if (it.config != Bitmap.Config.ARGB_8888) it.copy(Bitmap.Config.ARGB_8888, false) else it
        }
        val file = File(File(context.cacheDir, "shared").apply { mkdirs() }, name)
        file.outputStream().use { image.compress(Bitmap.CompressFormat.PNG, 100, it) }
        FileProvider.getUriForFile(context, "${context.packageName}.share", file)
    }
    // The clip carries the read grant to the chooser's preview as well as to the app picked.
    val send = Intent(Intent.ACTION_SEND).setType("image/png").putExtra(Intent.EXTRA_STREAM, uri)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    send.clipData = ClipData.newRawUri(null, uri)
    context.startActivity(Intent.createChooser(send, translate("Share")))
}
