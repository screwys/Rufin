package io.github.screwys.rufin.browse

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.shared.library.*

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SmartPlaylistEditor(
    model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit,
    playlist: SmartPlaylistRow? = null, folder: Long? = null,
) {
    val library = browse.library ?: return
    val sources = model.sources?.sources.orEmpty()
    var name by remember { mutableStateOf(playlist?.name.orEmpty()) }
    var definition by remember { mutableStateOf(playlist?.definition ?: library.smartPlaylistDefaultDefinition()) }
    var nextId by remember { mutableIntStateOf(0) }
    var all by remember { mutableStateOf(definition.matchAll.map { nextId++ to it }) }
    var any by remember { mutableStateOf(definition.matchAny.map { nextId++ to it }) }
    val invalid = remember { mutableStateMapOf<Int, Boolean>() }
    var limit by remember { mutableStateOf(definition.limit?.toString().orEmpty()) }
    val validLimit = limit.isBlank() || limit.toULongOrNull()?.let { it > 0UL } == true
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate(if (playlist == null) "New Smart Playlist" else "Edit Smart Playlist"), style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(name, { name = it }, label = { Text(translate("Playlist name")) }, modifier = Modifier.fillMaxWidth(), singleLine = true)
            SmartChoice(translate("Source"), (listOf<String?>(null) + sources.map { it.id } + listOfNotNull(definition.sourceId)).distinct(),
                definition.sourceId, { id -> sources.firstOrNull { it.id == id }?.name ?: id ?: translate("All") }) {
                definition = definition.copy(sourceId = it)
            }
            Text(translate("All"), style = MaterialTheme.typography.titleSmall)
            all.forEach { (id, rule) -> key(id) {
                SmartRuleEditor(rule, library, { changed -> all = all.map { if (it.first == id) id to changed else it } },
                    { invalid[id] = !it }, { all = all.filter { it.first != id }; invalid.remove(id) })
            } }
            TextButton({ all = all + (nextId++ to library.smartPlaylistRule(SmartPlaylistRuleField.TITLE, null)) }) { Text(translate("Add Rule")) }
            Text(translate("Any"), style = MaterialTheme.typography.titleSmall)
            any.forEach { (id, rule) -> key(id) {
                SmartRuleEditor(rule, library, { changed -> any = any.map { if (it.first == id) id to changed else it } },
                    { invalid[id] = !it }, { any = any.filter { it.first != id }; invalid.remove(id) })
            } }
            TextButton({ any = any + (nextId++ to library.smartPlaylistRule(SmartPlaylistRuleField.TITLE, null)) }) { Text(translate("Add Rule")) }
            SmartChoice(translate("Sort"), SmartPlaylistSort.entries, definition.sortField, { smartLabel(it.name) }) { definition = definition.copy(sortField = it) }
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Descending"), Modifier.weight(1f))
                Switch(definition.descending, { definition = definition.copy(descending = it) })
            }
            SmartChoice(translate("Period"), SmartPlaylistActivityPeriod.entries, definition.activityPeriod, { smartLabel(it.name) }) {
                definition = definition.copy(activityPeriod = it)
            }
            OutlinedTextField(limit, { limit = it }, label = { Text(translate("Limit")) }, singleLine = true,
                isError = !validLimit, modifier = Modifier.fillMaxWidth(),
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = androidx.compose.ui.text.input.KeyboardType.Number))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onDismiss) { Text(translate("Cancel")) }
                TextButton({ model.runAction {
                    library.saveSmartPlaylist(playlist?.smartPlaylistKey, name.trim(), definition.copy(
                        matchAll = all.map { it.second }, matchAny = any.map { it.second }, limit = limit.toULongOrNull()), folder)
                    browse.refresh()
                    onDismiss()
                } }, enabled = name.isNotBlank() && validLimit && invalid.values.none { it } && model.pendingActions == 0) {
                    Text(translate(if (playlist == null) "Create" else "Save"))
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
private fun SmartRuleEditor(
    rule: SmartPlaylistRule, library: io.github.screwys.rufin.core.AndroidLibrary,
    change: (SmartPlaylistRule) -> Unit, valid: (Boolean) -> Unit, remove: () -> Unit,
) {
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f)) {
                    SmartChoice(translate("Field"), SmartPlaylistRuleField.entries, rule.field, { smartLabel(it.name) }) {
                        valid(true)
                        change(library.smartPlaylistRule(it, null))
                    }
                }
                PlayerIconButton("rufin-user-trash-symbolic", "Remove", remove)
            }
            SmartChoice(translate("Condition"), library.smartPlaylistRuleOperators(rule.field), rule.operator, { smartLabel(it.name) }) {
                valid(true)
                change(library.smartPlaylistRule(rule.field, it))
            }
            key(rule.field, rule.operator) {
                when (val value = rule.value) {
                    is SmartPlaylistRuleValue.Text -> OutlinedTextField(value.v1, { change(rule.copy(value = value.copy(v1 = it))) },
                        label = { Text(translate("Text")) }, singleLine = true, modifier = Modifier.fillMaxWidth())
                    is SmartPlaylistRuleValue.Date -> OutlinedTextField(value.v1, { change(rule.copy(value = value.copy(v1 = it))) },
                        label = { Text(translate("Date")) }, placeholder = { Text("YYYY-MM-DD") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                    is SmartPlaylistRuleValue.Bool -> Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text(smartLabel(rule.field.name), Modifier.weight(1f))
                        Switch(value.v1, { change(rule.copy(value = value.copy(v1 = it))) })
                    }
                    is SmartPlaylistRuleValue.Number -> SmartNumber(smartLabel(rule.field.name), value.v1, valid, rating = rule.field == SmartPlaylistRuleField.RATING) { change(rule.copy(value = value.copy(v1 = it))) }
                    is SmartPlaylistRuleValue.NumberRange -> {
                        var minValid by remember { mutableStateOf(true) }
                        var maxValid by remember { mutableStateOf(true) }
                        SmartNumber(translate("Minimum"), value.min, { minValid = it; valid(it && maxValid) }, rating = rule.field == SmartPlaylistRuleField.RATING) { change(rule.copy(value = value.copy(min = it))) }
                        SmartNumber(translate("Maximum"), value.max, { maxValid = it; valid(it && minValid) }, rating = rule.field == SmartPlaylistRuleField.RATING) { change(rule.copy(value = value.copy(max = it))) }
                    }
                    is SmartPlaylistRuleValue.DateRange -> {
                        OutlinedTextField(value.start, { change(rule.copy(value = value.copy(start = it))) }, label = { Text(translate("Date")) },
                            placeholder = { Text("YYYY-MM-DD") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                        Text(translate("to"))
                        OutlinedTextField(value.end, { change(rule.copy(value = value.copy(end = it))) }, label = { Text(translate("Date")) },
                            placeholder = { Text("YYYY-MM-DD") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                    }
                    null -> Unit
                }
            }
        }
    }
}

@Composable
private fun SmartNumber(label: String, value: Long, valid: (Boolean) -> Unit, rating: Boolean = false, change: (Long) -> Unit) {
    var text by remember { mutableStateOf(if (rating) (value / 2.0).toString() else value.toString()) }
    fun parse(input: String): Long? = if (rating) input.toDoubleOrNull()?.times(2)?.let {
        if (it in 0.0..10.0 && it % 1.0 == 0.0) it.toLong() else null
    } else input.toLongOrNull()
    OutlinedTextField(text, {
        text = it
        val parsed = parse(it)
        valid(parsed != null)
        parsed?.let(change)
    }, label = { Text(label) }, isError = parse(text) == null, singleLine = true,
        modifier = Modifier.fillMaxWidth(),
        keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType =
            if (rating) androidx.compose.ui.text.input.KeyboardType.Decimal else androidx.compose.ui.text.input.KeyboardType.Number))
}

@Composable
private fun <T> SmartChoice(label: String, options: List<T>, selected: T, title: (T) -> String, choose: (T) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        OutlinedButton({ expanded = true }, Modifier.fillMaxWidth()) {
            Text(label, Modifier.weight(1f))
            Text(title(selected))
        }
        DropdownMenu(expanded, { expanded = false }) {
            options.forEach { option -> DropdownMenuItem(text = { Text(title(option)) }, onClick = { choose(option); expanded = false }) }
        }
    }
}

private fun smartLabel(name: String): String = when (name) {
    "TITLE" -> translate("Title")
    "ARTIST" -> translate("Artist")
    "ALBUM" -> translate("Album")
    "COMMENT" -> translate("Comment")
    "GENRE" -> translate("Genre")
    "MOOD" -> translate("Mood")
    "BPM" -> translate("BPM")
    "RATING" -> translate("Rating")
    "YEAR" -> translate("Year")
    "FAVORITE" -> translate("Favorite")
    "PLAYED" -> translate("Ever played")
    "PLAY_COUNT" -> translate("Play count")
    "SKIP_COUNT" -> translate("Skip count")
    "LAST_PLAYED" -> translate("Last played")
    "DATE_ADDED" -> translate("Date added")
    "DURATION" -> translate("Duration")
    "CONTAINS" -> translate("contains")
    "NOT_CONTAINS" -> translate("does not contain")
    "EQUALS" -> translate("equals")
    "NOT_EQUALS" -> translate("does not equal")
    "ABOVE" -> translate("above")
    "BELOW" -> translate("below")
    "BETWEEN" -> translate("range")
    "IS" -> translate("is")
    "IS_NOT" -> translate("is not")
    "BEFORE" -> translate("before")
    "AFTER" -> translate("after")
    "IS_EMPTY" -> translate("is empty")
    "IS_NOT_EMPTY" -> translate("is not empty")
    "WEEKLY" -> translate("Weekly")
    "MONTHLY" -> translate("Monthly")
    "YEARLY" -> translate("Yearly")
    "LIFETIME" -> translate("All time")
    else -> name
}
