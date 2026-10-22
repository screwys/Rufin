package io.github.screwys.rufin.metadata

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import kotlinx.coroutines.launch
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.metadata_lookup.MetadataMatchKind

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MetadataLookupDialog(connection: MetadataConnection) {
    var query by remember(connection.lookupIndex) { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val review = connection.lookupReview
    val item = connection.lookupItem
    val progress = connection.lookupProgress
    val stage = progress?.stage.orEmpty()
    val progressText = if (stage.isEmpty()) {
        translate("{completed} of {total} ({cached} cached)")
    } else {
        translate("{stage} ({completed} of {total}, {cached} cached)").replace("{stage}", stage)
    }.replace("{completed}", (progress?.completed ?: 0UL).toString())
        .replace("{total}", (progress?.total ?: 1UL).toString())
        .replace("{cached}", (progress?.cached ?: 0UL).toString())
    val editable = !connection.lookupBusy
    Dialog(onDismissRequest = { if (!connection.lookupAccepting) connection.cancelLookup() },
        properties = DialogProperties(usePlatformDefaultWidth = false, dismissOnClickOutside = false,
            dismissOnBackPress = !connection.lookupAccepting)) {
        Surface(Modifier.fillMaxSize()) {
            ModalNavigationDrawer(drawerState = drawer, gesturesEnabled = (progress?.total ?: 0UL) > 1UL, drawerContent = {
                review?.takeIf { (progress?.total ?: 0UL) > 1UL }?.let { review -> ModalDrawerSheet {
                    MetadataTracks(review, progress!!.total, review.scopeTitle(), false, connection.lookupAllTracks,
                        connection.selectedLookupTracks, if (progress.finished) ULong.MAX_VALUE else progress.completed / 64UL,
                        load = { offset, limit -> connection.lookupTracks(offset, limit) }, onScope = null,
                        onAll = connection::selectAllLookupTracks,
                        onTrack = { index, toggle ->
                            connection.selectLookupTrack(index, toggle)
                            if (!toggle) scope.launch { drawer.close() }
                        })
                } }
            }) {
            Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(Modifier.fillMaxWidth()) {
                    Text(translate("Lookup"), Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                    if ((progress?.total ?: 0UL) > 1UL) TextButton({ scope.launch { drawer.open() } }) { Text(translate("Tracks")) }
                }
                Text(progressText, style = MaterialTheme.typography.labelMedium)
                item?.let { Text(it.title, style = MaterialTheme.typography.titleMedium) }
                if (progress?.finished == false) LinearProgressIndicator(
                    progress = { progress.completed.toFloat() / progress.total.toFloat() }, modifier = Modifier.fillMaxWidth())
                val failure = connection.lookupFailure?.message ?: progress?.failure ?: item?.failure
                failure?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    val canSearch = !connection.lookupAccepting && (item?.ready == true || progress?.finished == true)
                    OutlinedTextField(query, { query = it; connection.searchLookup(it, delayed = true) }, Modifier.weight(1f), singleLine = true,
                        label = { Text(translate("Search MusicBrainz")) }, enabled = canSearch,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        keyboardActions = KeyboardActions(onSearch = { connection.searchLookup(query) }))
                    IconButton({ connection.refreshLookup(query) }, enabled = canSearch) {
                        RufinIcon("rufin-view-refresh-symbolic", translate("Refresh"))
                    }
                }
                LazyColumn(Modifier.weight(1f)) {
                    item {
                        ListItem(headlineContent = { Text(translate("Original")) },
                            colors = ListItemDefaults.colors(containerColor = if (item?.selected == null)
                                MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface),
                            modifier = Modifier.clickable(enabled = editable && item?.ready == true) { connection.chooseLookup(null) })
                    }
                    items(item?.candidates.orEmpty(), key = { it.id }) { candidate ->
                        ListItem(headlineContent = { Text(candidate.title) }, supportingContent = { Text(candidate.detail) },
                            trailingContent = { Text(when (candidate.kind) {
                                MetadataMatchKind.RECORDING -> translate("Recording")
                                MetadataMatchKind.RELEASE -> translate("Release")
                                MetadataMatchKind.RELEASE_GROUP -> translate("Release group")
                                MetadataMatchKind.ARTIST -> translate("Artist")
                            }, style = MaterialTheme.typography.labelSmall) },
                            colors = ListItemDefaults.colors(containerColor = if (item?.selected == candidate.id)
                                MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface),
                            modifier = Modifier.clickable(enabled = editable) { connection.chooseLookup(candidate) })
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(connection.lookupFillMissing, connection::updateLookupFillMissing, enabled = editable)
                    Text(translate("Only fill missing information"))
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(translate("Original"), Modifier.weight(1f), style = MaterialTheme.typography.labelLarge)
                    Text(translate("Suggested"), Modifier.weight(1f), style = MaterialTheme.typography.labelLarge)
                }
                LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    items(item?.fields.orEmpty().filter { it.original.isNotEmpty() || it.suggested.isNotEmpty() }, key = { it.label }) { field ->
                        Column {
                            Text(field.label, style = MaterialTheme.typography.labelMedium)
                            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                Text(field.original, Modifier.weight(1f), color = MaterialTheme.colorScheme.onSurfaceVariant)
                                Text(field.suggested, Modifier.weight(1f), color = if (field.changed)
                                    MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                            }
                        }
                    }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Spacer(Modifier.weight(1f))
                    if (progress?.finished == false) TextButton(connection::stopLookup) { Text(translate("Stop")) }
                    TextButton(connection::cancelLookup, enabled = !connection.lookupAccepting) { Text(translate("Cancel")) }
                    Button(connection::acceptLookup, enabled = editable && connection.lookupSelectedCount > 0UL) { Text(translate("Done")) }
                }
            }
        }
        }
    }
}
