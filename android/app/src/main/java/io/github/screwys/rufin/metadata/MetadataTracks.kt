package io.github.screwys.rufin.metadata

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.rufin_core.MetadataTrackSummary

@Composable
internal fun MetadataTracks(
    owner: Any, count: ULong, title: String?, scopeActive: Boolean,
    all: Boolean, selected: Set<ULong>, revision: ULong,
    load: suspend (ULong, ULong) -> List<MetadataTrackSummary>,
    onScope: (() -> Unit)?, onAll: (() -> Unit)?, onTrack: (ULong, Boolean) -> Unit,
) {
    val scroll = rememberLazyListState()
    val start by remember { derivedStateOf { (scroll.firstVisibleItemIndex / 64) * 64 } }
    var rows by remember(owner) { mutableStateOf(emptyMap<ULong, MetadataTrackSummary>()) }
    val currentLoader by rememberUpdatedState(load)
    LaunchedEffect(owner, start, revision) {
        rows = currentLoader(start.toULong(), 128UL).associateBy { it.index }
    }
    Column(Modifier.widthIn(max = 320.dp).fillMaxHeight().padding(8.dp)) {
        title?.let {
            if (onScope != null) NavigationDrawerItem(label = { Text(it, maxLines = 2, overflow = TextOverflow.Ellipsis) }, selected = scopeActive, onClick = onScope)
            else Text(it, Modifier.padding(16.dp), style = MaterialTheme.typography.titleMedium)
        }
        onAll?.let { TextButton(it, Modifier.fillMaxWidth()) { Text(translate("Select all")) } }
        LazyColumn(state = scroll, modifier = Modifier.weight(1f)) {
            items(count.coerceAtMost(Int.MAX_VALUE.toULong()).toInt(), key = { it }) { index ->
                val row = rows[index.toULong()]
                val checked = !scopeActive && if (all) index.toULong() !in selected else index.toULong() in selected
                NavigationDrawerItem(
                    label = {
                        Column {
                            Text(row?.title.orEmpty(), maxLines = 2, overflow = TextOverflow.Ellipsis,
                                color = if (row?.modified == true) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                            row?.scanStatus?.takeIf(String::isNotEmpty)?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
                        }
                    }, selected = checked,
                    badge = if (onAll != null) ({ Checkbox(checked, { onTrack(index.toULong(), true) }) }) else null,
                    onClick = { onTrack(index.toULong(), false) },
                )
            }
        }
    }
}
