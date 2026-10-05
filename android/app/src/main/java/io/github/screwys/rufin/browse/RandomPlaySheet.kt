package io.github.screwys.rufin.browse

import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow

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
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.paging.*
import androidx.paging.compose.collectAsLazyPagingItems
import androidx.paging.compose.itemKey
import io.github.screwys.rufin.core.translate
import kotlinx.coroutines.CancellationException
import org.json.JSONObject

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun RandomPlaySheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit) {
    val library = browse.library
    var settings by remember { mutableStateOf<String?>(null) }
    var limit by remember { mutableStateOf("") }
    var minYear by remember { mutableStateOf("") }
    var maxYear by remember { mutableStateOf("") }
    var minEnabled by remember { mutableStateOf(false) }
    var maxEnabled by remember { mutableStateOf(false) }
    var filter by remember { mutableStateOf("All") }
    var genreTitle by remember { mutableStateOf("") }
    var genres by remember { mutableStateOf(false) }
    var playedFilter by remember { mutableStateOf(false) }
    LaunchedEffect(library) {
        try {
            val raw = library?.randomSettings() ?: return@LaunchedEffect
            val saved = JSONObject(raw)
            settings = raw
            limit = saved.getInt("limit").toString()
            minEnabled = !saved.isNull("min_year")
            maxEnabled = !saved.isNull("max_year")
            minYear = if (minEnabled) saved.getInt("min_year").toString() else ""
            maxYear = if (maxEnabled) saved.getInt("max_year").toString() else ""
            filter = saved.getString("played_filter")
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
    val valid = settings != null && limit.toIntOrNull() != null &&
        (!minEnabled || minYear.toIntOrNull() != null) && (!maxEnabled || maxYear.toIntOrNull() != null)
    fun play(placement: String) {
        model.runAction {
            val saved = JSONObject(settings!!)
            saved.put("limit", limit.toInt())
            saved.put("min_year", if (minEnabled) minYear.toInt() else JSONObject.NULL)
            saved.put("max_year", if (maxEnabled) maxYear.toInt() else JSONObject.NULL)
            saved.put("played_filter", filter)
            model.connectedService().playRequest {
                val queued = library?.randomPlay(saved.toString(), placement) == true
                if (queued) onDismiss() else error(translate("No matching tracks found"))
            }
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).imePadding().padding(horizontal = 16.dp, vertical = 24.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp)) {
            Text(translate("Play random"), style = MaterialTheme.typography.titleLarge)
            if (settings == null) Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            OutlinedTextField(limit, { limit = it }, label = { Text(translate("Number of songs")) },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), singleLine = true,
                enabled = settings != null, modifier = Modifier.fillMaxWidth())
            RandomYearInput("Minimum year", minYear, { minYear = it }, minEnabled, { minEnabled = it })
            RandomYearInput("Maximum year", maxYear, { maxYear = it }, maxEnabled, { maxEnabled = it })
            SettingsGroup {
            SettingsRow(translate("Genre"), summary = genreTitle,
                trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(24.dp)) },
                modifier = Modifier.clickable(enabled = settings != null) { genres = true })
            SettingsRow(translate("Play filter"), summary = translate(when (filter) {
                "Unplayed" -> "Only unplayed tracks"
                "Played" -> "Only played tracks"
                else -> "All tracks"
            }), trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(24.dp)) },
                modifier = Modifier.clickable(enabled = settings != null) { playedFilter = true })
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton({ play("next") }, enabled = valid && model.pendingActions == 0) {
                    RufinIcon("rufin-mail-forward-symbolic", null, Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(translate("Play Next"))
                }
                Button({ play("now") }, enabled = valid && model.pendingActions == 0) {
                    RufinIcon("rufin-media-playback-start-symbolic", null, Modifier.size(20.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(translate("Play"))
                }
                OutlinedButton({ play("append") }, enabled = valid && model.pendingActions == 0) {
                    RufinIcon("rufin-go-last-symbolic", null, Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(translate("Play Later"))
                }
            }
        }
    }
    if (playedFilter) ModalBottomSheet(onDismissRequest = { playedFilter = false }) {
        Text(translate("Play filter"), Modifier.padding(24.dp), style = MaterialTheme.typography.titleLarge)
        listOf("All" to "All tracks", "Unplayed" to "Only unplayed tracks", "Played" to "Only played tracks").forEach { (id, title) ->
            ListItem(headlineContent = { Text(translate(title)) }, leadingContent = { RadioButton(filter == id, null) },
                modifier = Modifier.clickable { filter = id; playedFilter = false })
        }
        Spacer(Modifier.height(24.dp))
    }
    if (genres) RandomGenreSheet(model, browse, { genres = false }) { route ->
        model.runAction {
            settings = library?.randomGenre(settings!!, route)
            genres = false
        }
    }
}

@Composable
private fun RandomYearInput(label: String, year: String, setYear: (String) -> Unit,
    enabled: Boolean, setEnabled: (Boolean) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Switch(enabled, setEnabled)
        OutlinedTextField(year, setYear, label = { Text(translate(label)) }, enabled = enabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), singleLine = true, modifier = Modifier.weight(1f))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RandomGenreSheet(model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit,
    select: (String?) -> Unit,
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
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(translate("Genre"), Modifier.padding(24.dp), style = MaterialTheme.typography.titleLarge)
        OutlinedTextField(filter, { filter = it }, label = { Text(translate("Search")) }, singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp))
        BrowseMenuAction("rufin-audio-x-generic-symbolic", "Any genre") { select(null) }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), contentPadding = PaddingValues(bottom = 24.dp)) {
            items(rows.itemCount, key = rows.itemKey { it.key }) { index ->
                val row = rows[index]
                if (row == null) Spacer(Modifier.height(56.dp))
                else ListItem(headlineContent = { Text(row.title) }, modifier = Modifier.clickable { select(row.detailRoute) })
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
