package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.*
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.CancellationException
import io.github.screwys.rufin.shared.rufin_core.MixSettings
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.library.MixKind
import io.github.screwys.rufin.shared.rufin_core.MixKindChoice
import io.github.screwys.rufin.settings.LocalTranslationRevision
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role

/** The counts and the years the sheet offers, as GTK's. */
private val mixCounts = 1UL..500UL
private val mixYears = 1850..2050
/** The kind tiles a page holds, in three columns, as GTK's. */
private const val kindsPerPage = 6
private const val kindColumns = 3

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun MixSheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit) {
    val library = browse.library
    var settings by remember { mutableStateOf<MixSettings?>(null) }
    var albums by remember { mutableStateOf(false) }
    // Tracks and albums each keep their own count; the row shows the chosen unit's.
    var limit by remember { mutableStateOf(mixCounts.first) }
    var albumLimit by remember { mutableStateOf(mixCounts.first) }
    var minYear by remember { mutableStateOf<UShort?>(null) }
    var maxYear by remember { mutableStateOf<UShort?>(null) }
    var kind by remember { mutableStateOf(MixKind.ALL) }
    var favorites by remember { mutableStateOf(false) }
    var genreTitle by remember { mutableStateOf("") }
    var genres by remember { mutableStateOf(false) }
    var years by remember { mutableStateOf(false) }
    // Core's kinds in the order its pages show them, named in the chosen language.
    val kinds = remember(library, LocalTranslationRevision.current) { library?.mixKinds().orEmpty() }
    val pages = (kinds.size + kindsPerPage - 1) / kindsPerPage
    val pager = rememberPagerState { pages }
    LaunchedEffect(library) {
        try {
            if (library == null) return@LaunchedEffect
            val saved = model.connectedService().runtime.value!!.getOrThrow().settings().use { it.mixPreferences() }
            settings = saved
            albums = saved.albums
            limit = saved.limit.coerceIn(mixCounts)
            albumLimit = saved.albumLimit.coerceIn(mixCounts)
            minYear = saved.minYear
            maxYear = saved.maxYear
            kind = saved.kind
            favorites = saved.favorites
            // The page that holds the saved kind opens.
            kinds.indexOfFirst { it.kind == saved.kind }.takeIf { it >= 0 }?.let { pager.scrollToPage(it / kindsPerPage) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model.runAction { throw error } }
    }
    LaunchedEffect(library, settings) {
        settings?.let { raw ->
            try { genreTitle = library?.mixGenreTitle(raw).orEmpty() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { model.runAction { throw error } }
        }
    }
    fun play(placement: QueuePlacement) {
        model.runAction {
            val saved = settings!!.copy(albums = albums, limit = limit, albumLimit = albumLimit,
                minYear = minYear, maxYear = maxYear, kind = kind, favorites = favorites)
            model.connectedService().playRequest {
                val empty = library?.mix(saved, placement) != false
                if (!empty) onDismiss() else error(translate("No matching tracks found"))
            }
        }
    }
    val loaded = settings != null
    val scheme = MaterialTheme.colorScheme
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        val inset = Modifier.fillMaxWidth().padding(horizontal = 16.dp)
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)) {
            // The title, with Tracks and Albums beside it.
            Row(inset.heightIn(min = 40.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Quick Mix"), Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                SingleChoiceSegmentedButtonRow(Modifier.width(176.dp)) {
                    listOf(false to "Tracks", true to "Albums").forEachIndexed { index, (unit, label) ->
                        // Both halves are filled, the chosen one in the accent.
                        SegmentedButton(albums == unit, { albums = unit }, SegmentedButtonDefaults.itemShape(index, 2), enabled = loaded, icon = {},
                            colors = SegmentedButtonDefaults.colors(activeContainerColor = scheme.primary,
                                activeContentColor = scheme.onPrimary, inactiveContainerColor = scheme.surfaceContainerHighest,
                                inactiveContentColor = scheme.onSurface, disabledInactiveContainerColor = scheme.surfaceContainer),
                            border = SegmentedButtonDefaults.borderStroke(Color.Transparent)) { Text(translate(label), maxLines = 1) }
                    }
                }
            }
            // The kinds, six glyph tiles a page; a swipe pages through them.
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                HorizontalPager(pager, Modifier.fillMaxWidth(), contentPadding = PaddingValues(horizontal = 16.dp), pageSpacing = 32.dp,
                    verticalAlignment = Alignment.Top) { page ->
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        kinds.drop(page * kindsPerPage).take(kindsPerPage).chunked(kindColumns).forEach { row ->
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                row.forEach { choice -> MixKindTile(choice, kind == choice.kind, loaded, Modifier.weight(1f)) { kind = choice.kind } }
                                repeat(kindColumns - row.size) { Spacer(Modifier.weight(1f)) }
                            }
                        }
                    }
                }
                if (pages > 1) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally)) {
                    repeat(pages) { page ->
                        val current = pager.currentPage == page
                        Box(Modifier.size(if (current) 18.dp else 6.dp, 6.dp).clip(CircleShape)
                            .background(if (current) scheme.primary else scheme.onSurface.copy(alpha = .2f)))
                    }
                }
            }
            // Genre and Years open their pickers; Favorites only turns on and off.
            FlowRow(inset, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                val genre = settings?.genre != null
                MixChip(if (genre) genreTitle.ifEmpty { translate("Genre") } else translate("Genre"), genre, loaded, trailing = "rufin-pan-down-symbolic") { genres = true }
                val ranged = minYear != null || maxYear != null
                MixChip(if (ranged) listOfNotNull(minYear?.toString(), "–", maxYear?.toString()).joinToString(" ") else translate("Years"),
                    ranged, loaded, trailing = "rufin-pan-down-symbolic") { years = true }
                // Picking tracks of a kind whose rules pick favorites alone, Favorites only is on and
                // can't change; the choice stays as it was for the other kinds.
                val fixed = !albums && kinds.firstOrNull { it.kind == kind }?.favorites == true
                val shown = fixed || favorites
                MixChip(translate("Favorites only"), shown, loaded && !fixed,
                    leading = if (shown) "rufin-object-select-symbolic" else "rufin-heart-outline-symbolic") { favorites = !favorites }
            }
            // The count of the chosen unit.
            Row(inset.heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(translate(if (albums) "Albums" else "Tracks"), style = MaterialTheme.typography.bodyLarge)
                    if (albums) Text(translate("Each plays whole"), style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant)
                }
                val count = if (albums) albumLimit else limit
                fun step(next: ULong) { if (albums) albumLimit = next else limit = next }
                MixStep("rufin-value-decrease-symbolic", "Less", loaded && count > mixCounts.first) { step(count - 1UL) }
                Text(count.toString(), Modifier.widthIn(min = 48.dp), textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.SemiBold, color = scheme.onSurface)
                MixStep("rufin-list-add-symbolic", "More", loaded && count < mixCounts.last) { step(count + 1UL) }
            }
            // Play, the one filled button, then Play Next and Play Later, each icon beside its label.
            val ready = loaded && model.pendingActions == 0
            // Each at its own width, at the end; a long translation wraps a button to the next line.
            FlowRow(inset, horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                MixPlay("rufin-media-playback-start-symbolic", "Play", true, ready) { play(QueuePlacement.Replace(0UL)) }
                MixPlay("rufin-mail-forward-symbolic", "Play Next", false, ready) { play(QueuePlacement.AfterCurrent) }
                MixPlay("rufin-go-last-symbolic", "Play Later", false, ready) { play(QueuePlacement.End) }
            }
        }
    }
    if (genres) MixGenreSheet(model, browse, { genres = false }) { route ->
        model.runAction {
            settings = settings?.copy(genre = library?.mixGenre(route))
            genres = false
        }
    }
    if (years) MixYearsDialog(minYear, maxYear, { years = false }) { min, max ->
        minYear = min
        maxYear = max
        years = false
    }
}

/** A kind: its glyph over its name, filled a shade apart from the sheet; the chosen one takes the accent's ring. */
@Composable
private fun MixKindTile(choice: MixKindChoice, chosen: Boolean, enabled: Boolean, modifier: Modifier, choose: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val shape = RoundedCornerShape(16.dp)
    Column(modifier.height(76.dp).clip(shape).background(if (chosen) scheme.secondaryContainer else scheme.surfaceContainerHigh)
        .then(if (chosen) Modifier.border(2.dp, scheme.primary, shape) else Modifier)
        .selectable(chosen, enabled, Role.RadioButton, onClick = choose).padding(horizontal = 6.dp, vertical = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterVertically)) {
        val tint = if (chosen) scheme.onSecondaryContainer else scheme.onSurface
        RufinIcon(choice.icon, null, Modifier.size(22.dp), tint)
        Text(choice.title, style = MaterialTheme.typography.labelMedium, color = tint, textAlign = TextAlign.Center,
            maxLines = 2, overflow = TextOverflow.Ellipsis)
    }
}

/** A filter chip of the sheet: filled, and in the tonal accent while it narrows the mix. */
@Composable
private fun MixChip(label: String, active: Boolean, enabled: Boolean, leading: String? = null, trailing: String? = null, action: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    // A chip another choice fixes is dimmed, as the sheet's disabled buttons are.
    Surface(action, Modifier.height(36.dp).alpha(if (enabled) 1f else .38f), enabled, RoundedCornerShape(8.dp),
        color = if (active) scheme.secondaryContainer else scheme.surfaceContainerHighest,
        contentColor = if (active) scheme.onSecondaryContainer else scheme.onSurface) {
        Row(Modifier.padding(start = if (leading != null) 10.dp else 14.dp, end = if (trailing != null) 8.dp else 14.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            leading?.let { RufinIcon(it, null, Modifier.size(16.dp)) }
            Text(label, Modifier.widthIn(max = 200.dp), style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            trailing?.let { RufinIcon(it, null, Modifier.size(16.dp)) }
        }
    }
}

/** A play button of the sheet: its icon beside its label; Play is the filled one. */
@Composable
private fun MixPlay(icon: String, label: String, primary: Boolean, enabled: Boolean, modifier: Modifier = Modifier, action: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Surface(action, modifier.heightIn(min = 52.dp).alpha(if (enabled) 1f else .38f), enabled, RoundedCornerShape(16.dp),
        color = if (primary) scheme.primary else scheme.surfaceContainerHigh,
        contentColor = if (primary) scheme.onPrimary else scheme.onSurface) {
        Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
            verticalAlignment = Alignment.CenterVertically) {
            RufinIcon(icon, null, Modifier.size(20.dp), glyph = true)
            Text(translate(label), style = if (primary) MaterialTheme.typography.labelLarge.copy(fontWeight = FontWeight.SemiBold)
                else MaterialTheme.typography.labelLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** A step of the count: a tonal round button, a shade apart from the sheet. */
@Composable
private fun MixStep(icon: String, label: String, enabled: Boolean, action: () -> Unit) {
    FilledTonalIconButton(action, Modifier.size(40.dp), enabled) {
        RufinIcon(icon, translate(label), Modifier.size(18.dp), glyph = true)
    }
}

/** Edits both years; an empty or unreadable one leaves that end open. */
@Composable
private fun MixYearsDialog(minYear: UShort?, maxYear: UShort?, dismiss: () -> Unit, apply: (UShort?, UShort?) -> Unit) {
    var min by remember { mutableStateOf(minYear?.toString().orEmpty()) }
    var max by remember { mutableStateOf(maxYear?.toString().orEmpty()) }
    fun year(text: String) = text.toUShortOrNull()?.takeIf { it.toInt() in mixYears }
    AlertDialog(onDismissRequest = dismiss, title = { Text(translate("Years")) }, text = {
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            listOf(Triple("Minimum year", min) { text: String -> min = text }, Triple("Maximum year", max) { text: String -> max = text },
            ).forEach { (label, text, set) ->
                OutlinedTextField(text, { set(it.filter(Char::isDigit).take(4)) }, Modifier.fillMaxWidth(),
                    label = { Text(translate(label)) }, placeholder = { Text(translate("Any")) },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), singleLine = true)
            }
        }
    }, confirmButton = { TextButton({ apply(year(min), year(max)) }) { Text(translate("Apply")) } },
        dismissButton = { TextButton(dismiss) { Text(translate("Cancel")) } })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MixGenreSheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit,
    select: (Route?) -> Unit,
) {
    var filter by remember { mutableStateOf("") }
    val library = browse.library
    val route = browse.routes.firstOrNull { it.id == "Genres" }?.route
    val rows = remember(library, route, filter) {
        Pager(PagingConfig(pageSize = 64, initialLoadSize = 64, prefetchDistance = 24,
            maxSize = 192, enablePlaceholders = true)) { BrowseChoiceSource(library, route, filter) }.flow
    }.collectAsLazyPagingItems()
    val loadError = (rows.loadState.refresh as? LoadState.Error)?.error ?: (rows.loadState.append as? LoadState.Error)?.error
    LaunchedEffect(loadError) { loadError?.let { error -> model.runAction { throw error } } }
    // Fully open and of one height, so the list keeps its place while the search reads again
    // and stays above the keyboard.
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        // The rows are the sheet's color, as a plain list of names.
        val rowColors = ListItemDefaults.colors(containerColor = Color.Transparent)
        Column(Modifier.fillMaxWidth().fillMaxHeight(.9f).imePadding()) {
            Text(translate("Genre"), Modifier.padding(horizontal = 24.dp), style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(filter, { filter = it }, label = { Text(translate("Search")) }, singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp))
            LazyColumn(Modifier.fillMaxWidth().weight(1f), contentPadding = PaddingValues(bottom = 24.dp)) {
                // Any genre leads the list as a row like the genres under it.
                if (filter.isBlank()) item(key = "any") {
                    ListItem(headlineContent = { Text(translate("Any")) }, colors = rowColors, modifier = Modifier.clickable { select(null) })
                }
                items(rows.itemCount, key = rows.itemKey { it.row.key }) { index ->
                    val row = rows[index]?.row
                    if (row == null) Spacer(Modifier.height(56.dp))
                    else ListItem(headlineContent = { Text(row.title) }, colors = rowColors, modifier = Modifier.clickable { select(row.route) })
                }
                if (rows.loadState.refresh is LoadState.Loading || rows.loadState.append is LoadState.Loading) item {
                    Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                }
                loadError?.let { failure -> item {
                    ErrorRow(failure, translate("Retry"), rows::retry)
                } }
            }
        }
    }
}
