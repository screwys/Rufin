package io.github.screwys.rufin.browse

import io.github.screwys.rufin.shared.library.QueuePlacement
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsGroup
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
import io.github.screwys.rufin.shared.rufin_core.RandomPlaySettings
import io.github.screwys.rufin.shared.rufin_core.Route
import io.github.screwys.rufin.shared.library.PlayedFilter

/** The counts and the years the sheet offers, as GTK's. */
private val mixCounts = 1UL..500UL
private val mixYears = 1850..2050

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun RandomPlaySheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit) {
    val library = browse.library
    var settings by remember { mutableStateOf<RandomPlaySettings?>(null) }
    var albums by remember { mutableStateOf(false) }
    // Tracks and albums each keep their own count; the row shows the chosen unit's.
    var limit by remember { mutableStateOf(mixCounts.first) }
    var albumLimit by remember { mutableStateOf(mixCounts.first) }
    var minYear by remember { mutableStateOf<UShort?>(null) }
    var maxYear by remember { mutableStateOf<UShort?>(null) }
    var filter by remember { mutableStateOf(PlayedFilter.ALL) }
    var genreTitle by remember { mutableStateOf("") }
    var genres by remember { mutableStateOf(false) }
    var years by remember { mutableStateOf(false) }
    LaunchedEffect(library) {
        try {
            if (library == null) return@LaunchedEffect
            val saved = model.connectedService().runtime.value!!.getOrThrow().settings().use { it.randomPreferences() }
            settings = saved
            albums = saved.albums
            limit = saved.limit.coerceIn(mixCounts)
            albumLimit = saved.albumLimit.coerceIn(mixCounts)
            minYear = saved.minYear
            maxYear = saved.maxYear
            filter = saved.playedFilter
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model.runAction { throw error } }
    }
    LaunchedEffect(library, settings) {
        settings?.let { raw ->
            try { genreTitle = library?.randomGenreTitle(raw).orEmpty() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { model.runAction { throw error } }
        }
    }
    fun play(placement: QueuePlacement) {
        model.runAction {
            val saved = settings!!.copy(albums = albums, limit = limit, albumLimit = albumLimit,
                minYear = minYear, maxYear = maxYear, playedFilter = filter)
            model.connectedService().playRequest {
                val empty = library?.randomPlay(saved, placement) != false
                if (!empty) onDismiss() else error(translate("No matching tracks found"))
            }
        }
    }
    val loaded = settings != null
    val scheme = MaterialTheme.colorScheme
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        // The rows reach the sheet's edges; everything else keeps the rows' inset.
        val inset = Modifier.fillMaxWidth().padding(horizontal = 16.dp)
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text(translate("Mix"), inset, style = MaterialTheme.typography.titleLarge)
            SingleChoiceSegmentedButtonRow(inset) {
                listOf(false to "Tracks", true to "Albums").forEachIndexed { index, (unit, label) ->
                    // Both halves are filled, the chosen one as the chosen chip below.
                    SegmentedButton(albums == unit, { albums = unit }, SegmentedButtonDefaults.itemShape(index, 2), enabled = loaded, icon = {},
                        colors = SegmentedButtonDefaults.colors(activeContainerColor = scheme.primary,
                            activeContentColor = scheme.onPrimary, inactiveContainerColor = scheme.surfaceContainerHighest,
                            inactiveContentColor = scheme.onSurface, disabledInactiveContainerColor = scheme.surfaceContainer),
                        border = SegmentedButtonDefaults.borderStroke(Color.Transparent)) { Text(translate(label)) }
                }
            }
            FlowRow(inset, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(PlayedFilter.NOT_PLAYED_RECENTLY to "Not played recently", PlayedFilter.ALL to "Random",
                    PlayedFilter.UNPLAYED to "Unplayed", PlayedFilter.PLAYED to "Played", PlayedFilter.RECENTLY_ADDED to "Recently added",
                ).forEach { (kind, label) ->
                    // Filled as Rufin's chips; the chosen kind takes the accent, so the one choice stands out.
                    FilterChip(filter == kind, { filter = kind }, { Text(translate(label)) }, Modifier.height(36.dp), loaded, border = null,
                        colors = FilterChipDefaults.filterChipColors(containerColor = scheme.surfaceContainerHighest,
                            labelColor = scheme.onSurface, selectedContainerColor = scheme.primary, selectedLabelColor = scheme.onPrimary,
                            disabledContainerColor = scheme.surfaceContainer, disabledSelectedContainerColor = scheme.surfaceContainer))
                }
            }
            // The three settings as one filled group, as a settings page lists its rows.
            Box(inset) { SettingsGroup {
                MixRow(translate("Genre"), Modifier.clickable(enabled = loaded) { genres = true }) {
                    MixValue(if (settings?.genre == null) translate("Any") else genreTitle)
                }
                MixRow(translate("Years"), Modifier.clickable(enabled = loaded) { years = true }) {
                    MixValue(if (minYear == null && maxYear == null) translate("Any")
                        else listOfNotNull(minYear?.toString(), "–", maxYear?.toString()).joinToString(" "))
                }
                MixRow(translate(if (albums) "Albums" else "Tracks"),
                    supporting = if (albums) translate("Each plays whole") else null) {
                    val count = if (albums) albumLimit else limit
                    fun step(next: ULong) { if (albums) albumLimit = next else limit = next }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        MixStep("rufin-value-decrease-symbolic", "Less", loaded && count > mixCounts.first) { step(count - 1UL) }
                        Text(count.toString(), Modifier.widthIn(min = 44.dp), textAlign = TextAlign.Center,
                            style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.SemiBold,
                            color = MaterialTheme.colorScheme.onSurface)
                        MixStep("rufin-list-add-symbolic", "More", loaded && count < mixCounts.last) { step(count + 1UL) }
                    }
                }
            } }
            // Play, Play Next and Play Later as the track menu's big buttons.
            val ready = loaded && model.pendingActions == 0
            Row(inset, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                BrowsePrimaryAction("rufin-media-playback-start-symbolic", "Play", Modifier.weight(1f), enabled = ready) { play(QueuePlacement.Replace(0UL)) }
                BrowsePrimaryAction("rufin-mail-forward-symbolic", "Play Next", Modifier.weight(1f), enabled = ready) { play(QueuePlacement.AfterCurrent) }
                BrowsePrimaryAction("rufin-go-last-symbolic", "Play Later", Modifier.weight(1f), enabled = ready) { play(QueuePlacement.End) }
            }
        }
    }
    if (genres) RandomGenreSheet(model, browse, { genres = false }) { route ->
        model.runAction {
            settings = settings?.copy(genre = library?.randomGenre(route))
            genres = false
        }
    }
    if (years) MixYearsDialog(minYear, maxYear, { years = false }) { min, max ->
        minYear = min
        maxYear = max
        years = false
    }
}

/** A row of the sheet: its title, with what it is set to or its control at the end. */
@Composable
private fun MixRow(title: String, modifier: Modifier = Modifier, supporting: String? = null, trailing: @Composable () -> Unit) {
    ListItem(headlineContent = { Text(title) }, supportingContent = supporting?.let { { Text(it) } }, trailingContent = trailing,
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        modifier = Modifier.clip(RoundedCornerShape(5.dp)).then(modifier))
}

/** A step of the count: a tonal round button, a shade apart from the row it sits on. */
@Composable
private fun MixStep(icon: String, label: String, enabled: Boolean, action: () -> Unit) {
    FilledTonalIconButton(action, Modifier.size(40.dp), enabled) {
        RufinIcon(icon, translate(label), Modifier.size(18.dp), glyph = true)
    }
}

@Composable
private fun MixValue(value: String) {
    Text(value, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
        maxLines = 1, overflow = TextOverflow.Ellipsis)
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
private fun RandomGenreSheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit,
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
