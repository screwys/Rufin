package io.github.screwys.rufin.diagnostics

import io.github.screwys.rufin.app.RufinConnection

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.*

@Composable
internal fun TroubleshootingPage(model: RufinConnection, save: () -> Unit) {
    val context = LocalContext.current
    val state = rememberLazyListState()
    val lines = remember { mutableStateListOf<String>() }
    LaunchedEffect(model.service, model.ready) {
        val runtime = model.service?.runtime?.value?.getOrNull() ?: return@LaunchedEffect
        lines.clear()
        var cursor = 0uL
        var startCharacter = 0uL
        while (isActive) {
            val update = withContext(Dispatchers.IO) { runtime.diagnosticUpdates(cursor) }
            if (update.end != cursor) {
                val tail = state.layoutInfo.totalItemsCount - 1
                val follow = tail <= 0 || state.layoutInfo.visibleItemsInfo.lastOrNull()?.index == tail
                var removed = update.startCharacter - startCharacter
                var removedLines = 0
                while (removedLines < lines.size && removed > 0uL) {
                    val line = lines[removedLines]
                    val characters = line.codePointCount(0, line.length).toULong()
                    if (characters > removed) {
                        lines[removedLines] = line.substring(line.offsetByCodePoints(0, removed.toInt()))
                        break
                    }
                    removed -= characters
                    removedLines++
                }
                lines.subList(0, removedLines).clear()
                val added = update.text.split('\n')
                for ((index, part) in added.withIndex()) {
                    val line = part + if (index < added.lastIndex) "\n" else ""
                    if (line.isEmpty()) continue
                    if (lines.lastOrNull()?.endsWith('\n') == false) lines[lines.lastIndex] += line
                    else lines.add(line)
                }
                cursor = update.end
                startCharacter = update.startCharacter
                if (follow && lines.isNotEmpty()) state.scrollToItem(lines.lastIndex)
            }
            delay(250)
        }
    }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(translate("Debug logging"), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
            Switch(model.debugLogging, model::changeDebugLogging, enabled = model.ready)
            IconButton({ (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Rufin", lines.joinToString(""))) }) {
                RufinIcon("rufin-edit-copy-symbolic", translate("Copy"))
            }
            IconButton(save, enabled = model.ready) { RufinIcon("rufin-document-save-symbolic", translate("Save")) }
        }
        Text(translate("Logs have secrets and absolute folder paths redacted, but you may still want to review the logs before sharing them"),
            Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.bodySmall)
        SelectionContainer(Modifier.weight(1f)) {
            LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp), state = state, contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = LocalPlayerBottomPadding.current)) {
                items(lines.size) { index -> Text(lines[index].removeSuffix("\n"), fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}
