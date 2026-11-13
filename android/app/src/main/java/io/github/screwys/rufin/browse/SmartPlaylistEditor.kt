package io.github.screwys.rufin.browse

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.AndroidLibrary
import io.github.screwys.rufin.core.AndroidSmartNumberBounds
import io.github.screwys.rufin.core.AndroidSmartPlaylistTemplate
import io.github.screwys.rufin.core.smartFieldSection
import io.github.screwys.rufin.core.smartFieldSectionTitle
import io.github.screwys.rufin.core.smartMatchTitle
import io.github.screwys.rufin.core.smartNumberBounds
import io.github.screwys.rufin.core.smartNumberScale
import io.github.screwys.rufin.core.smartOperatorTitle
import io.github.screwys.rufin.core.smartPeriodTitle
import io.github.screwys.rufin.core.smartPlaylistTemplates
import io.github.screwys.rufin.core.smartReadsPlays
import io.github.screwys.rufin.core.smartRuleFieldTitle
import io.github.screwys.rufin.core.smartSortTitle
import io.github.screwys.rufin.core.smartValueKind
import io.github.screwys.rufin.core.smartValuePlaceholder
import io.github.screwys.rufin.core.trackCountText
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.settingsTextFieldColors
import io.github.screwys.rufin.shared.library.*
import io.github.screwys.rufin.ui.ChoiceChip
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay

/** A rule or a group of the list being edited, with an id that keys its card and validity. */
private sealed interface SmartPart { val id: Int }
private data class SmartRulePart(override val id: Int, val rule: SmartPlaylistRule) : SmartPart
private data class SmartGroupPart(override val id: Int, val matching: SmartPlaylistMatch, val rules: List<SmartRulePart>) : SmartPart

private fun SmartPart.condition(): SmartPlaylistCondition = when (this) {
    is SmartRulePart -> SmartPlaylistCondition.Rule(rule)
    is SmartGroupPart -> SmartPlaylistCondition.Group(matching, rules.map { it.rule })
}

/** The text fields whose values the library suggests while typing. */
private val suggestedFields = setOf(SmartPlaylistRuleField.ARTIST, SmartPlaylistRuleField.ALBUM,
    SmartPlaylistRuleField.GENRE, SmartPlaylistRuleField.MOOD)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SmartPlaylistEditor(
    model: RufinConnection, browse: BrowseConnection, onDismiss: () -> Unit,
    playlist: SmartPlaylistRow? = null, folder: Long? = null,
) {
    val library = browse.library ?: return
    val sources = model.sources?.sources.orEmpty()
    val templates = remember { if (playlist == null) smartPlaylistTemplates() else emptyList() }
    var name by remember { mutableStateOf(playlist?.name.orEmpty()) }
    var definition by remember { mutableStateOf(playlist?.definition ?: library.smartPlaylistDefaultDefinition()) }
    var nextId by remember { mutableIntStateOf(0) }
    fun rulePart(rule: SmartPlaylistRule) = SmartRulePart(nextId++, rule)
    fun partsOf(conditions: List<SmartPlaylistCondition>): List<SmartPart> = conditions.map {
        when (it) {
            is SmartPlaylistCondition.Rule -> rulePart(it.v1)
            is SmartPlaylistCondition.Group -> SmartGroupPart(nextId++, it.matching, it.rules.map(::rulePart))
        }
    }
    var parts by remember { mutableStateOf(partsOf(definition.conditions)) }
    val invalid = remember { mutableStateMapOf<Int, Boolean>() }
    var limit by remember { mutableStateOf(definition.limit?.toString().orEmpty()) }
    val validLimit = limit.isBlank() || limit.toULongOrNull()?.let { it > 0UL } == true
    val edited = definition.copy(conditions = parts.map { it.condition() }, limit = limit.toULongOrNull())
    var count by remember { mutableStateOf<ULong?>(null) }
    LaunchedEffect(edited) {
        delay(250)
        count = try { library.smartPlaylistMatchCount(edited) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { null }
    }
    fun newRule() = rulePart(library.smartPlaylistRule(SmartPlaylistRuleField.TITLE, null))
    fun apply(template: AndroidSmartPlaylistTemplate) {
        val start = template.definition
        definition = definition.copy(matching = start.matching, sortField = start.sortField, descending = start.descending,
            activityPeriod = start.activityPeriod)
        limit = start.limit?.toString().orEmpty()
        invalid.clear()
        parts = partsOf(start.conditions)
        if (name.isBlank()) name = template.name
    }
    @Composable
    fun RuleCard(part: SmartRulePart, change: (SmartPlaylistRule) -> Unit, remove: () -> Unit) =
        SmartRuleCard(part.rule, library, edited.sourceId, change, { invalid[part.id] = !it }, { invalid.remove(part.id); remove() })

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Row(Modifier.fillMaxWidth().padding(start = 24.dp, end = 16.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Column(Modifier.weight(1f)) {
                Text(translate(if (playlist == null) "New Smart Playlist" else "Edit Smart Playlist"), style = MaterialTheme.typography.titleLarge)
                Text(count?.let(::trackCountText).orEmpty(), style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Button({ model.runAction {
                library.saveSmartPlaylist(playlist?.smartPlaylistKey, name.trim(), edited, folder)
                onDismiss()
            } }, enabled = name.isNotBlank() && validLimit && invalid.values.none { it } && model.pendingActions == 0) {
                Text(translate(if (playlist == null) "Create" else "Save"))
            }
        }
        Column(Modifier.fillMaxWidth().imePadding().verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp)) {
            OutlinedTextField(name, { name = it }, label = { Text(translate("Playlist name")) }, modifier = Modifier.fillMaxWidth(), singleLine = true)
            if (templates.isNotEmpty()) Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(translate("Start from"), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    templates.forEach { template -> ChoiceChip(false, { apply(template) }, { Text(template.name) }) }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(translate("Match"))
                SmartMatchChoice(definition.matching, capitalized = false) { definition = definition.copy(matching = it) }
                Text(translate("of these rules"))
            }
            parts.forEach { part -> key(part.id) {
                when (part) {
                    is SmartRulePart -> RuleCard(part, { changed -> parts = parts.map { if (it.id == part.id) part.copy(rule = changed) else it } },
                        { parts = parts.filter { it.id != part.id } })
                    is SmartGroupPart -> {
                        fun changeGroup(changed: SmartGroupPart) { parts = parts.map { if (it.id == part.id) changed else it } }
                        Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = RoundedCornerShape(16.dp)) {
                            Column(Modifier.fillMaxWidth().padding(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                                Row(Modifier.padding(start = 4.dp), verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    SmartMatchChoice(part.matching, capitalized = true) { changeGroup(part.copy(matching = it)) }
                                    Text(translate("of these"), Modifier.weight(1f))
                                    PlayerIconButton("rufin-user-trash-symbolic", "Remove", {
                                        part.rules.forEach { invalid.remove(it.id) }
                                        parts = parts.filter { it.id != part.id }
                                    })
                                }
                                part.rules.forEach { rule -> key(rule.id) {
                                    RuleCard(rule, { changed -> changeGroup(part.copy(rules = part.rules.map { if (it.id == rule.id) rule.copy(rule = changed) else it })) },
                                        { changeGroup(part.copy(rules = part.rules.filter { it.id != rule.id })) })
                                } }
                                SmartAddButton("Add rule") { changeGroup(part.copy(rules = part.rules + newRule())) }
                            }
                        }
                    }
                }
            } }
            Row(Modifier.offset(x = (-8).dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                SmartAddButton("Add rule") { parts = parts + newRule() }
                SmartAddButton("Add group") { parts = parts + SmartGroupPart(nextId++, SmartPlaylistMatch.ALL, listOf(newRule())) }
            }
            SettingsGroup(translate("Order")) {
                SmartSortRow(definition.sortField, definition.descending) { sort, descending ->
                    definition = definition.copy(sortField = sort, descending = descending)
                }
                SettingsRow(translate("Limit"), trailing = {
                    OutlinedTextField(limit, { limit = it }, Modifier.width(128.dp), singleLine = true, isError = !validLimit,
                        placeholder = { Text(translate("No limit")) }, colors = settingsTextFieldColors(),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number))
                })
                if (smartReadsPlays(edited)) PreferenceChoice(translate("Plays from"), definition.activityPeriod,
                    SmartPlaylistActivityPeriod.entries.map { it to smartPeriodTitle(it) }) { definition = definition.copy(activityPeriod = it) }
                if (sources.size >= 2 || definition.sourceId != null) PreferenceChoice(translate("Source"), definition.sourceId,
                    (listOf<String?>(null) + sources.map { it.id } + listOfNotNull(definition.sourceId)).distinct().map { id ->
                        id to (sources.firstOrNull { it.id == id }?.name ?: id ?: translate("All"))
                    }) { definition = definition.copy(sourceId = it) }
            }
        }
    }
}

@Composable
private fun SmartAddButton(label: String, add: () -> Unit) {
    TextButton(add) {
        RufinIcon("rufin-list-add-symbolic", null, Modifier.size(18.dp))
        Spacer(Modifier.width(8.dp))
        Text(translate(label))
    }
}

@Composable
private fun SmartMatchChoice(matching: SmartPlaylistMatch, capitalized: Boolean, choose: (SmartPlaylistMatch) -> Unit) {
    // A group's header starts with the choice, so it has its own capitalized strings, as on GTK.
    fun title(option: SmartPlaylistMatch) = when {
        !capitalized -> smartMatchTitle(option)
        option == SmartPlaylistMatch.ALL -> translate("All")
        else -> translate("Any")
    }
    var open by remember { mutableStateOf(false) }
    Box {
        FilledTonalButton({ open = true }, Modifier.height(32.dp), contentPadding = PaddingValues(start = 12.dp, end = 8.dp)) {
            Text(title(matching))
            RufinIcon("rufin-pan-down-symbolic", null, Modifier.padding(start = 4.dp).size(14.dp))
        }
        DropdownMenu(open, { open = false }) {
            SmartPlaylistMatch.entries.forEach { option ->
                DropdownMenuItem(text = { Text(title(option)) }, onClick = { choose(option); open = false })
            }
        }
    }
}

@Composable
private fun SmartSortRow(sort: SmartPlaylistSort, descending: Boolean, choose: (SmartPlaylistSort, Boolean) -> Unit) {
    var open by remember { mutableStateOf(false) }
    @Composable
    fun Item(title: String, selected: Boolean, pick: () -> Unit) = DropdownMenuItem(text = { Text(title) },
        leadingIcon = { if (selected) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp)) else Spacer(Modifier.size(18.dp)) },
        onClick = { pick(); open = false })
    val random = sort == SmartPlaylistSort.RANDOM
    Box {
        SettingsRow(translate("Sort by"),
            summary = smartSortTitle(sort) + if (random) "" else ", " + translate(if (descending) "Descending" else "Ascending"),
            trailing = { RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(20.dp)) },
            modifier = Modifier.clickable { open = true })
        DropdownMenu(open, { open = false }) {
            SmartPlaylistSort.entries.forEach { option -> Item(smartSortTitle(option), option == sort) { choose(option, descending) } }
            if (!random) {
                HorizontalDivider()
                Item(translate("Ascending"), !descending) { choose(sort, false) }
                Item(translate("Descending"), descending) { choose(sort, true) }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SmartRuleCard(
    rule: SmartPlaylistRule, library: AndroidLibrary, source: String?,
    change: (SmartPlaylistRule) -> Unit, valid: (Boolean) -> Unit, remove: () -> Unit,
) {
    OutlinedCard(Modifier.fillMaxWidth(), colors = CardDefaults.outlinedCardColors(containerColor = Color.Transparent)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                SmartMenuField(translate("Field"), smartRuleFieldTitle(rule.field), Modifier.weight(1f)) { close ->
                    SmartPlaylistRuleField.entries.groupBy(::smartFieldSection).forEach { (section, fields) ->
                        Text(smartFieldSectionTitle(section), Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                        fields.forEach { field -> DropdownMenuItem(text = { Text(smartRuleFieldTitle(field)) }, onClick = {
                            close()
                            valid(true)
                            change(library.smartPlaylistRule(field, null))
                        }) }
                    }
                }
                SmartMenuField(translate("Condition"), smartOperatorTitle(rule.field, rule.operator), Modifier.weight(1f)) { close ->
                    library.smartPlaylistRuleOperators(rule.field).forEach { operator ->
                        DropdownMenuItem(text = { Text(smartOperatorTitle(rule.field, operator)) }, onClick = {
                            close()
                            valid(true)
                            change(library.smartPlaylistRule(rule.field, operator))
                        })
                    }
                }
                PlayerIconButton("rufin-user-trash-symbolic", "Remove", remove)
            }
            key(rule.field, rule.operator) { SmartRuleValue(rule, library, source, change, valid) }
        }
    }
}

@Composable
private fun SmartRuleValue(
    rule: SmartPlaylistRule, library: AndroidLibrary, source: String?,
    change: (SmartPlaylistRule) -> Unit, valid: (Boolean) -> Unit,
) {
    val bounds = remember { smartNumberBounds(rule.field, rule.operator) }
    val scale = remember { smartNumberScale(rule.field) }
    when (val value = rule.value) {
        is SmartPlaylistRuleValue.Text -> {
            val label = smartValuePlaceholder(rule.field)
            if (rule.field in suggestedFields) SmartSuggestedText(label, value.v1, rule.field, library, source) { change(rule.copy(value = value.copy(v1 = it))) }
            else OutlinedTextField(value.v1, { change(rule.copy(value = value.copy(v1 = it))) },
                label = { Text(label) }, singleLine = true, modifier = Modifier.fillMaxWidth())
        }
        is SmartPlaylistRuleValue.Playlist -> SmartPlaylistPicker(value.v1, library, source) { change(rule.copy(value = value.copy(v1 = it))) }
        is SmartPlaylistRuleValue.Date -> OutlinedTextField(value.v1, { change(rule.copy(value = value.copy(v1 = it))) },
            label = { Text(translate("Date")) }, placeholder = { Text("YYYY-MM-DD") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        is SmartPlaylistRuleValue.Bool -> Row(Modifier.fillMaxWidth().padding(start = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(smartRuleFieldTitle(rule.field), Modifier.weight(1f))
            Switch(value.v1, { change(rule.copy(value = value.copy(v1 = it))) })
        }
        is SmartPlaylistRuleValue.Number -> {
            val days = smartValueKind(rule.field, rule.operator) == SmartPlaylistRuleValueKind.DAYS
            SmartNumber(if (days) translate("days") else smartRuleFieldTitle(rule.field), value.v1, bounds, scale, valid) {
                change(rule.copy(value = value.copy(v1 = it)))
            }
        }
        is SmartPlaylistRuleValue.NumberRange -> {
            var minValid by remember { mutableStateOf(true) }
            var maxValid by remember { mutableStateOf(true) }
            SmartNumber(translate("Minimum"), value.min, bounds, scale, { minValid = it; valid(it && maxValid) }) { change(rule.copy(value = value.copy(min = it))) }
            SmartNumber(translate("Maximum"), value.max, bounds, scale, { maxValid = it; valid(it && minValid) }) { change(rule.copy(value = value.copy(max = it))) }
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

/** A number in shown units: `scale` stored units are one shown unit, as ratings in stars. */
@Composable
private fun SmartNumber(label: String, value: Long, bounds: AndroidSmartNumberBounds, scale: Long, valid: (Boolean) -> Unit, change: (Long) -> Unit) {
    var text by remember { mutableStateOf(value.toBigDecimal().divide(scale.toBigDecimal()).stripTrailingZeros().toPlainString()) }
    fun parse(input: String): Long? = input.trim().toBigDecimalOrNull()?.multiply(scale.toBigDecimal())
        ?.let { runCatching { it.longValueExact() }.getOrNull() }?.takeIf { it in bounds.min..bounds.max }
    OutlinedTextField(text, {
        text = it
        val parsed = parse(it)
        valid(parsed != null)
        parsed?.let(change)
    }, label = { Text(label) }, isError = parse(text) == null, singleLine = true, modifier = Modifier.fillMaxWidth(),
        keyboardOptions = KeyboardOptions(keyboardType = if (scale > 1) KeyboardType.Decimal else KeyboardType.Number))
}

/** A read-only field that opens a menu of choices. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SmartMenuField(label: String, value: String, modifier: Modifier, content: @Composable ColumnScope.(close: () -> Unit) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded, { expanded = it }, modifier) {
        OutlinedTextField(value, {}, Modifier.menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
            readOnly = true, singleLine = true, label = { Text(label) }, textStyle = MaterialTheme.typography.bodyMedium,
            trailingIcon = { RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp)) })
        ExposedDropdownMenu(expanded, { expanded = false }, matchAnchorWidth = false) { content { expanded = false } }
    }
}

/** The library's values under an exposed field, in a lazy list: the core sends at most 200. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ExposedDropdownMenuBoxScope.SmartValueMenu(
    expanded: Boolean, dismiss: () -> Unit, width: Int, choices: List<SmartPlaylistValueChoice>, pick: (SmartPlaylistValueChoice) -> Unit,
) {
    ExposedDropdownMenu(expanded && choices.isNotEmpty(), dismiss) {
        // The menu measures its content's intrinsic width, which a lazy list cannot give; a fixed width answers it.
        LazyColumn(Modifier.width(with(LocalDensity.current) { width.toDp() }).heightIn(max = 280.dp)) {
            items(choices, key = { it.value }) { choice ->
                DropdownMenuItem(text = { Text(choice.name, maxLines = 1) },
                    trailingIcon = { Text(choice.tracks.toString(), style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant) },
                    onClick = { pick(choice) })
            }
        }
    }
}

/** Free text with the library's matching values suggested while typing. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SmartSuggestedText(
    label: String, text: String, field: SmartPlaylistRuleField, library: AndroidLibrary, source: String?, change: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    var width by remember { mutableIntStateOf(0) }
    var choices by remember { mutableStateOf(emptyList<SmartPlaylistValueChoice>()) }
    LaunchedEffect(expanded, text, source) {
        if (!expanded) return@LaunchedEffect
        choices = try { library.smartPlaylistValues(field, text, source) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { emptyList() }
    }
    ExposedDropdownMenuBox(expanded, { expanded = it }) {
        OutlinedTextField(text, { change(it); expanded = true },
            Modifier.menuAnchor(ExposedDropdownMenuAnchorType.PrimaryEditable).fillMaxWidth()
                .onSizeChanged { width = it.width }.onFocusChanged { if (!it.isFocused) expanded = false },
            label = { Text(label) }, singleLine = true)
        SmartValueMenu(expanded, { expanded = false }, width, choices) { change(it.value); expanded = false }
    }
}

/** One of the library's playlists, stored by object id; a saved id no longer listed still shows. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SmartPlaylistPicker(id: String, library: AndroidLibrary, source: String?, change: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    var width by remember { mutableIntStateOf(0) }
    var choices by remember { mutableStateOf(emptyList<SmartPlaylistValueChoice>()) }
    LaunchedEffect(source) {
        choices = try { library.smartPlaylistValues(SmartPlaylistRuleField.PLAYLIST, "", source) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { emptyList() }
    }
    ExposedDropdownMenuBox(expanded, { expanded = it }) {
        OutlinedTextField(choices.firstOrNull { it.value == id }?.name ?: id, {},
            Modifier.menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable).fillMaxWidth().onSizeChanged { width = it.width },
            readOnly = true, singleLine = true, label = { Text(translate("Playlist")) },
            trailingIcon = { RufinIcon("rufin-pan-down-symbolic", null, Modifier.size(16.dp)) })
        SmartValueMenu(expanded, { expanded = false }, width, choices) { change(it.value); expanded = false }
    }
}
