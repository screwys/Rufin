package io.github.screwys.rufin.settings

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import io.github.screwys.rufin.shared.rufin_core.*

@Composable
internal fun ScrobblingPage(preferences: PreferencesConnection, embedded: Boolean = false) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state by remember { mutableStateOf<ScrobblingPreferences?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var pending by remember { mutableStateOf<String?>(null) }
    var edit by remember { mutableStateOf<String?>(null) }
    var connectToken by remember { mutableStateOf(false) }
    var value by remember { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    val linkColor = MaterialTheme.colorScheme.primary
    fun perform(service: String, feedback: String? = null, action: suspend (ScrobblingOwner) -> ScrobblingPreferences) {
        scope.launch { pending = service; failure = null
            try {
                val next = action(preferences.nativeScrobbling())
                state = next
                feedback?.let(preferences::showFeedback)
            }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error.message?.let(::translate) }
            finally { pending = null }
        }
    }
    fun openLink(url: String) {
        try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
        catch (error: Exception) { failure = error.message ?: translate("Could not open browser") }
    }
    LaunchedEffect(preferences) { perform("load") { it.loadPreferences() } }
    SettingsPage(embedded = embedded) {
        if (state == null && pending == "load") CircularProgressIndicator(Modifier.size(24.dp))
        if (edit == null) failure?.let { message -> io.github.screwys.rufin.ui.ErrorNotice(message) }
        state?.let { current ->
        for ((id, name) in listOf("lastfm" to "Last.fm", "librefm" to "Libre.fm", "listenbrainz" to "ListenBrainz")) {
            val enabled = when (id) { "lastfm" -> current.lastfm.enabled; "librefm" -> current.librefm.enabled; else -> current.listenbrainz.enabled }
            val nowPlaying = when (id) { "lastfm" -> current.lastfm.nowPlayingEnabled; "librefm" -> current.librefm.nowPlayingEnabled; else -> current.listenbrainz.nowPlayingEnabled }
            val username = when (id) { "lastfm" -> current.lastfm.username; "librefm" -> current.librefm.username; else -> current.listenbrainz.username }
            val connected = when (id) { "lastfm" -> current.lastfm.connected; "librefm" -> current.librefm.connected; else -> current.listenbrainz.userToken.isNotEmpty() && username.isNotEmpty() }
            fun update(enabled: Boolean, nowPlaying: Boolean) = when (id) {
                "lastfm" -> current.copy(lastfm = current.lastfm.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
                "librefm" -> current.copy(librefm = current.librefm.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
                else -> current.copy(listenbrainz = current.listenbrainz.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
            }
            SettingsGroup(name) {
            if (id == "lastfm" || id == "listenbrainz") {
                val help = buildAnnotatedString {
                    append(translate(if (id == "lastfm") "If you do not have API keys, create them" else "Find your ListenBrainz user token"))
                    append(" ")
                    val url = if (id == "lastfm") "https://www.last.fm/api/account/create" else "https://listenbrainz.org/settings/"
                    withLink(LinkAnnotation.Clickable(id, TextLinkStyles(SpanStyle(color = linkColor))) { openLink(url) }) {
                        append(translate("here"))
                    }
                    if (id == "lastfm") append(translate(". You only need to fill email and an application name parts"))
                    else append(".")
                }
                Text(help, Modifier.padding(horizontal = 16.dp, vertical = 12.dp), style = MaterialTheme.typography.bodyMedium)
            } else {
                Text(translate("If the page doesn't load, then Libre.fm blocks your IP range/VPN"),
                    Modifier.padding(horizontal = 16.dp, vertical = 12.dp), style = MaterialTheme.typography.bodyMedium)
            }
            SettingsSwitch(when (id) {
                "lastfm" -> translate("Last.fm scrobbling")
                "librefm" -> translate("Libre.fm scrobbling")
                else -> translate("ListenBrainz scrobbling")
            }, enabled) { enabled -> perform(id) { owner -> withContext(Dispatchers.IO) { owner.save(update(enabled, nowPlaying)) } } }
            SettingsSwitch(translate("Now playing updates"), nowPlaying) { value -> perform(id) { owner -> withContext(Dispatchers.IO) { owner.save(update(enabled, value)) } } }
            if (id != "librefm") SettingsRow(if (id == "lastfm") translate("API keys") else translate("User token"),
                modifier = Modifier.clickable {
                    value = if (id == "lastfm") current.lastfm.apiKey else current.listenbrainz.userToken; secret = current.lastfm.apiSecret; connectToken = false; edit = id
                })
            SettingsRow(translate("Connection"),
                summary = if (connected) {
                    username.takeIf(String::isNotBlank)?.let {
                        translate("Connected as {username}").replace("{username}", it)
                    } ?: translate("Connected")
                } else translate(if (id == "listenbrainz" && current.listenbrainz.userToken.isNotBlank()) "Saved" else "Not connected"),
                trailing = { if (pending == id) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) else TextButton({
                    if (id == "listenbrainz") {
                        value = current.listenbrainz.userToken; connectToken = true; edit = id
                    } else perform(id) { owner ->
                    val session = owner.connect(if (id == "lastfm") ScrobblingConnection.LastFm(current.lastfm.apiKey, current.lastfm.apiSecret) else ScrobblingConnection.LibreFm)
                    try { while (true) when (val event = withContext(Dispatchers.Default) { session.next() }) {
                        is ScrobblingConnectionEvent.OpenUrl -> {
                            var error: String? = null
                            try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(event.url))) }
                            catch (failure: Exception) { error = failure.message ?: translate("Could not open browser") }
                            event.opened.use { it.complete(error) }
                        }
                        is ScrobblingConnectionEvent.Connected -> {
                            preferences.showFeedback(translate("Connected as {username}").replace("{username}", event.username))
                            break
                        }
                        is ScrobblingConnectionEvent.Failed -> error(event.v1)
                        ScrobblingConnectionEvent.TimedOut -> error(translate("Authorization timed out"))
                    } } finally { session.destroy() }
                    owner.preferences()
                    }
                }, enabled = pending == null && state != null) { Text(translate(if (connected) "Reconnect" else "Connect")) } })
            }
        }
        }
    }
    edit?.let { id -> AlertDialog(onDismissRequest = { edit = null }, title = { Text(if (id == "lastfm") translate("API keys") else translate("User token")) },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            failure?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
            OutlinedTextField(value, { value = it }, label = { Text(if (id == "lastfm") translate("API key") else translate("User token")) }, singleLine = true, visualTransformation = PasswordVisualTransformation(), colors = settingsTextFieldColors())
            if (id == "lastfm") OutlinedTextField(secret, { secret = it }, label = { Text(translate("Shared secret")) }, singleLine = true, visualTransformation = PasswordVisualTransformation(), colors = settingsTextFieldColors())
        } }, confirmButton = { TextButton({
            if (id == "listenbrainz" && connectToken) perform(id) { owner ->
                val saved = owner.connectListenbrainz(value)
                val username = saved.listenbrainz.username
                preferences.showFeedback(translate("Connected as {username}").replace("{username}", username))
                edit = null
                saved
            } else perform(id, translate("Saved")) { owner ->
                val saved = owner.saveCredential(if (id == "lastfm") ScrobblingCredentialEdit.LastFm(value, secret) else ScrobblingCredentialEdit.ListenBrainz(value))
                edit = null
                saved
            }
        }, enabled = pending == null && (!connectToken || value.isNotBlank())) {
            if (pending == id) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
            else Text(translate(if (id == "listenbrainz" && connectToken) "Connect" else "Save"))
        } },
        dismissButton = { TextButton({ edit = null }) { Text(translate("Cancel")) } }) }
}
