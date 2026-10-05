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
    var log by remember { mutableStateOf("") }
    val lines = remember(log) { log.lines() }
    LaunchedEffect(model.service, model.ready) {
        val runtime = model.service?.runtime?.value?.getOrNull() ?: return@LaunchedEffect
        var revision = -1L
        while (isActive) {
            val current = runtime.diagnosticRevision().toLong()
            if (current != revision) {
                val tail = state.layoutInfo.totalItemsCount - 1
                val follow = tail <= 0 || state.layoutInfo.visibleItemsInfo.lastOrNull()?.index == tail
                log = withContext(Dispatchers.IO) { runtime.diagnosticLog() }
                revision = current
                if (follow && log.isNotBlank()) state.scrollToItem(log.lines().lastIndex)
            }
            delay(250)
        }
    }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(translate("Debug logging"), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
            Switch(model.debugLogging, model::changeDebugLogging, enabled = model.ready)
            IconButton({ (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Rufin", log)) }) {
                RufinIcon("rufin-edit-copy-symbolic", translate("Copy"))
            }
            IconButton(save, enabled = model.ready) { RufinIcon("rufin-document-save-symbolic", translate("Save")) }
        }
        Text(translate("Logs have secrets and absolute folder paths redacted, but you may still want to review the logs before sharing them"),
            Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.bodySmall)
        SelectionContainer(Modifier.weight(1f)) {
            LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp), state = state, contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = LocalPlayerBottomPadding.current)) {
                items(lines.size) { index -> Text(lines[index], fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}
