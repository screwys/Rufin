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
import org.json.JSONObject

@Composable
internal fun ScrobblingPage(preferences: PreferencesConnection, embedded: Boolean = false) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state by remember { mutableStateOf<JSONObject?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var pending by remember { mutableStateOf<String?>(null) }
    var edit by remember { mutableStateOf<String?>(null) }
    var connectToken by remember { mutableStateOf(false) }
    var value by remember { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    val linkColor = MaterialTheme.colorScheme.primary
    fun perform(service: String, feedback: String? = null, action: suspend (AndroidPreferences) -> String) {
        scope.launch { pending = service; failure = null
            try {
                val next = action(preferences.nativePreferences())
                state = JSONObject(next)
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
    LaunchedEffect(preferences) { perform("load") { it.scrobblingPreferences() } }
    SettingsPage(embedded = embedded) {
        if (state == null && pending == "load") CircularProgressIndicator(Modifier.size(24.dp))
        if (edit == null) failure?.let { message -> io.github.screwys.rufin.ui.ErrorNotice(message) }
        state?.let { current ->
        for ((id, name) in listOf("lastfm" to "Last.fm", "librefm" to "Libre.fm", "listenbrainz" to "ListenBrainz")) {
            val p = current.getJSONObject(id)
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
            }, p.optBoolean("enabled")) { enabled -> perform(id) { it.setScrobbling(id, enabled, p.optBoolean("now_playing", true)) } }
            SettingsSwitch(translate("Now playing updates"), p.optBoolean("now_playing", true)) { enabled -> perform(id) { it.setScrobbling(id, p.optBoolean("enabled"), enabled) } }
            if (id != "librefm") SettingsRow(if (id == "lastfm") translate("API keys") else translate("User token"),
                modifier = Modifier.clickable {
                    value = p.optString(if (id == "lastfm") "api_key" else "token"); secret = p.optString("api_secret"); connectToken = false; edit = id
                })
            SettingsRow(translate("Connection"),
                summary = if (p.optBoolean("connected")) {
                    p.optString("username").takeIf(String::isNotBlank)?.let {
                        translate("Connected as {username}").replace("{username}", it)
                    } ?: translate("Connected")
                } else translate(if (id == "listenbrainz" && p.optString("token").isNotBlank()) "Saved" else "Not connected"),
                trailing = { if (pending == id) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) else TextButton({
                    if (id == "listenbrainz") {
                        value = p.optString("token"); connectToken = true; edit = id
                    } else perform(id) { owner ->
                    val session = owner.authorizeScrobbling(id)
                    try { while (true) when (val event = session.next()) {
                        is AndroidScrobblingEvent.OpenBrowser -> {
                            var error: String? = null
                            try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(event.url))) }
                            catch (failure: Exception) { error = failure.message ?: translate("Could not open browser") }
                            session.browserOpened(error)
                        }
                        is AndroidScrobblingEvent.Connected -> {
                            preferences.showFeedback(translate("Connected as {username}").replace("{username}", event.username))
                            break
                        }
                        is AndroidScrobblingEvent.Failed -> error(event.reason)
                        AndroidScrobblingEvent.TimedOut -> error(translate("Authorization timed out"))
                    } } finally { session.destroy() }
                    owner.scrobblingPreferences()
                    }
                }, enabled = pending == null && state != null) { Text(translate(if (p.optBoolean("connected")) "Reconnect" else "Connect")) } })
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
                val username = JSONObject(saved).getJSONObject(id).optString("username")
                preferences.showFeedback(translate("Connected as {username}").replace("{username}", username))
                edit = null
                saved
            } else perform(id, translate("Saved")) { owner ->
                val saved = owner.setScrobblingCredential(id, value, secret)
                edit = null
                saved
            }
        }, enabled = pending == null && (!connectToken || value.isNotBlank())) {
            if (pending == id) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
            else Text(translate(if (id == "listenbrainz" && connectToken) "Connect" else "Save"))
        } },
        dismissButton = { TextButton({ edit = null }) { Text(translate("Cancel")) } }) }
}
