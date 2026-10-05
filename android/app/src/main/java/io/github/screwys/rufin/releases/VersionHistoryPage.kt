package io.github.screwys.rufin.releases

import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.more.ReleaseCheck
import io.github.screwys.rufin.more.MoreConnection

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.webkit.WebView
import android.webkit.WebViewClient
import android.webkit.WebResourceRequest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun VersionHistoryPage(more: MoreConnection) {
    val history = more.releases
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    var versions by remember { mutableStateOf(false) }
    val note = history?.notes?.firstOrNull { it.version == selected } ?: history?.notes?.firstOrNull()
    val context = LocalContext.current
    val colors = MaterialTheme.colorScheme
    fun css(color: androidx.compose.ui.graphics.Color) = "#%06x".format(color.toArgb() and 0xffffff)
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            TextButton({ versions = true }, Modifier.weight(1f)) { Text(note?.let { "v${it.version} · ${it.date}" } ?: translate("Version History")) }
            IconButton(more::checkReleases, enabled = more.bridge != null && more.releaseCheck != ReleaseCheck.Checking) {
                if (more.releaseCheck == ReleaseCheck.Checking) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                else RufinIcon(when (more.releaseCheck) {
                    ReleaseCheck.Success -> "rufin-object-select-symbolic"
                    ReleaseCheck.Error -> "rufin-window-close-symbolic"
                    else -> "rufin-view-refresh-symbolic"
                }, translate("Check for updates"))
            }
        }
        history?.let {
            Text("${translate("Installed")}: ${it.installedVersion}", Modifier.padding(horizontal = 16.dp), style = MaterialTheme.typography.bodySmall)
            it.error?.let { message -> io.github.screwys.rufin.ui.ErrorNotice(message) }
        }
        if (note == null) Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            if (more.bridge == null || more.releaseCheck == ReleaseCheck.Checking) CircularProgressIndicator()
            else Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(translate("No results"))
                TextButton(more::checkReleases) { Text(translate("Retry")) }
            }
        }
        else AndroidView(modifier = Modifier.weight(1f).fillMaxWidth()
            .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 16.dp + LocalPlayerBottomPadding.current)
            .clip(MaterialTheme.shapes.large), factory = { viewContext ->
            WebView(viewContext).apply {
                webViewClient = object : WebViewClient() {
                    override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                        context.startActivity(Intent(Intent.ACTION_VIEW, request.url)); return true
                    }
                }
            }
        }, update = { web: WebView ->
            val html = "<html><head><meta charset='utf-8'><style>body{background:${css(colors.surfaceContainerHigh)};color:${css(colors.onSurface)};font-family:sans-serif;padding:10px;line-height:1.5}a{color:${css(colors.primary)}}table{border-collapse:collapse}td,th{padding:6px}img{max-width:100%}</style></head><body>${note.html}</body></html>"
            val key = note.url to html
            if (web.tag != key) {
                web.tag = key
                web.loadDataWithBaseURL(note.url, html, "text/html", "UTF-8", null)
            }
        })
    }
    LaunchedEffect(more.bridge) { if (more.bridge != null && history?.notes.isNullOrEmpty()) more.checkReleases() }
    if (versions) ModalBottomSheet({ versions = false }) {
        LazyColumn {
            items(history?.notes.orEmpty(), key = { it.version }) { version ->
                ListItem(headlineContent = { Text("v${version.version}") }, supportingContent = { Text(version.date) },
                    trailingContent = { if (version.version == history?.installedVersion) Text(translate("Installed"), style = MaterialTheme.typography.labelSmall) },
                    modifier = Modifier.clickable { selected = version.version; versions = false })
            }
        }
    }
}
