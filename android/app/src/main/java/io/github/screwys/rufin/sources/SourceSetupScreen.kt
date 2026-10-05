package io.github.screwys.rufin.sources

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.settingsButtonColors
import kotlinx.coroutines.delay

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SourceSetupScreen(model: RufinConnection, onClose: () -> Unit, sourceId: String? = null, connectionsOnly: Boolean = false) {
    val setup = remember(model, sourceId) { model.sourceSetupSession(sourceId) }
    val form = setup.form
    var saved by remember(setup) { mutableStateOf(false) }
    LaunchedEffect(saved) {
        if (saved) { delay(2000); saved = false }
    }
    val context = LocalContext.current
    val close: () -> Unit = { model.closeSourceSetup(); onClose() }
    BackHandler(onBack = close)
    val chooseFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { setup.folder(it, close) }
    }
    val browser: (String) -> Unit = { url ->
        try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
        catch (error: Exception) { setup.reportError(error) }
    }
    val canSubmit = when (form.kind) {
        "plex" -> sourceId != null || setup.authorized && setup.choices.isNotEmpty()
        "emby" -> form.manualEmby && form.url.isNotBlank() || setup.authorized && setup.choices.isNotEmpty() || sourceId != null
        else -> setup.authorized || form.url.isNotBlank()
    }
    Scaffold(topBar = {
            TopAppBar(title = {
                Text(setup.providers.firstOrNull { it.kind == form.kind }?.title ?: translate(
                    if (sourceId != null) "Edit Source" else if (connectionsOnly) "File connection" else "Add Source"))
            }, navigationIcon = {
                IconButton({ if (sourceId == null && form.kind != null) { setup.cancelAuthorization(); form.kind = null } else close() }) {
                    RufinIcon("rufin-go-previous-symbolic", translate("Back"))
                }
            }, actions = {
                IconButton(close) { RufinIcon("rufin-window-close-symbolic", translate("Close")) }
            })
    }, bottomBar = {
        if (setup.ready && form.kind != null && form.kind != "local") {
            Surface {
                Button({
                    saved = false
                    setup.submit { if (sourceId == null) close() else saved = true }
                }, Modifier.fillMaxWidth().navigationBarsPadding().imePadding()
                    .padding(horizontal = 16.dp, vertical = 8.dp), enabled = !setup.working && canSubmit) {
                    Text(translate(if (sourceId == null) "Connect" else if (saved) "Saved..." else "Save"))
                }
            }
        }
    }) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding).imePadding()) {
            if (setup.working) LinearProgressIndicator(Modifier.fillMaxWidth())
            setup.failure?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
            if (!setup.ready) Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) { if (setup.failure == null) CircularProgressIndicator() }
            else if (form.kind == null) LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(16.dp)) {
                item { SettingsGroup {
                setup.providers.filter { !connectionsOnly || it.kind in setOf("webdav", "smb") }.forEach { provider ->
                    SettingsRow(provider.title, leading = { SourceIcon(provider.kind, Modifier.size(24.dp)) },
                        trailing = { RufinIcon("rufin-go-next-symbolic", null) }, modifier = Modifier.clickable {
                            setup.selectProvider(provider.kind)
                            if (connectionsOnly) setup.form.integration = true
                        })
                }
                } }
            } else if (form.kind == "local") Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (sourceId != null) SourceRatingSettings(setup)
                SettingsGroup {
                    SettingsRow(translate("Music"), leading = { RufinIcon("rufin-folders-symbolic", null) },
                        trailing = { RufinIcon("rufin-go-next-symbolic", null) },
                        modifier = Modifier.clickable(enabled = !setup.working) { setup.music(close) })
                    SettingsRow(translate("Choose folder"), leading = { RufinIcon("rufin-folders-symbolic", null) },
                        trailing = { RufinIcon("rufin-go-next-symbolic", null) },
                        modifier = Modifier.clickable(enabled = !setup.working) { chooseFolder.launch(null) })
                }
            } else LazyColumn(Modifier.weight(1f).fillMaxWidth(), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                item { SourceTextField("Name", form.name, { form.name = it }, !setup.working) }
                when (form.kind) {
                    "jellyfin", "emby" -> item { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        if (form.kind == "emby") SourceSwitch("Connect manually", form.manualEmby, {
                            setup.cancelAuthorization(); form.manualEmby = it
                        }, !setup.working)
                        if (form.kind == "jellyfin" || form.manualEmby) {
                            SourceCredentials(form, setup.working, sourceId != null)
                            SourceDiscovery(setup)
                            if (form.kind == "jellyfin") OutlinedButton({ setup.quickConnect(browser) }, enabled = !setup.working && form.url.isNotBlank(), colors = settingsButtonColors()) {
                                Text(translate("Quick Connect"))
                            }
                        } else {
                            SourceTextField("Username or email", form.username, { form.username = it }, !setup.working)
                            SourceTextField("Password", form.secret, { form.secret = it }, !setup.working, secret = true)
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                OutlinedButton({ setup.embyLogin(false, browser) }, enabled = !setup.working && form.username.isNotBlank(), colors = settingsButtonColors()) { Text(translate("Sign in")) }
                                OutlinedButton({ setup.embyLogin(true, browser) }, enabled = !setup.working, colors = settingsButtonColors()) { Text(translate("Sign in with a code")) }
                            }
                            SourceChoices(setup.choices, setup.selectedServer) { setup.selectedServer = it }
                        }
                        SourceSwitch("Use Instant Mix for recommendations", form.instantMix, { form.instantMix = it }, !setup.working)
                        if (form.kind == "jellyfin" || form.manualEmby) SourceSwitch("Verify server certificate", !form.trustInvalidCertificate, { form.trustInvalidCertificate = !it }, !setup.working)
                    } }
                    "navidrome", "subsonic" -> item { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        if (form.kind == "subsonic") SourceOptions("Authentication", listOf("password" to "Password", "legacy_password" to "Legacy password", "api_key" to "API key"), form.subsonicAuth, enabled = !setup.working) { form.subsonicAuth = it }
                        SourceCredentials(form, setup.working, sourceId != null, apiKey = form.subsonicAuth == "api_key")
                        SourceSwitch("Verify server certificate", !form.trustInvalidCertificate, { form.trustInvalidCertificate = !it }, !setup.working)
                    } }
                    "plex" -> item { PlexSourceForm(setup, sourceId != null, browser) }
                    "webdav", "smb" -> item { FileSourceForm(setup, sourceId != null, browser) }
                }
                if (sourceId != null && form.kind in setOf("navidrome", "subsonic")) item { SourceRatingSettings(setup) }
                item { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    setup.code?.let { code -> Text(code, style = MaterialTheme.typography.headlineLarge) }
                    setup.browserUrl?.takeUnless { setup.authorized || setup.choosingProfiles }?.let { url ->
                        Text(translate("Complete sign-in in your browser"))
                        TextButton({ browser(url) }) { RufinIcon("rufin-external-link-symbolic", null); Spacer(Modifier.width(8.dp)); Text(translate("Sign In in Browser")) }
                    }
                    if (setup.working && setup.browserUrl != null) TextButton(setup::cancelAuthorization) { Text(translate("Cancel")) }
                } }
            }
        }
    }
}
