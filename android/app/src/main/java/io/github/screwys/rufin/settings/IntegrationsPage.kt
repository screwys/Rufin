package io.github.screwys.rufin.settings

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.viewModelScope
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.folderCountText
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.shared.lyrics.ExternalLyricsProvider
import io.github.screwys.rufin.shared.metadata_lookup.ArtistImageProvider
import io.github.screwys.rufin.shared.metadata_lookup.CoverProvider
import io.github.screwys.rufin.shared.rufin_core.FileIntegration
import io.github.screwys.rufin.shared.rufin_core.ScrobblingConnection
import io.github.screwys.rufin.shared.rufin_core.ScrobblingConnectionEvent
import io.github.screwys.rufin.shared.rufin_core.ScrobblingOwner
import io.github.screwys.rufin.shared.rufin_core.ScrobblingPreferences
import io.github.screwys.rufin.shared.rufin_core.ScrobblingProtocol
import io.github.screwys.rufin.shared.rufin_core.ScrobblingServerRequest
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.sources.SourceSetupScreen
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SegmentedPill
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** An integration's page, as the Integrations list opens it: its id and name. */
internal data class IntegrationPage(val id: String, val name: String) : java.io.Serializable

/** The page id of an added scrobbling server. */
private const val SERVER = "server:"
private const val LASTFM_API_CREATE_URL = "https://www.last.fm/api/account/create"
private const val LISTENBRAINZ_TOKEN_URL = "https://listenbrainz.org/settings/"
private const val FANART_KEY_URL = "https://fanart.tv/get-an-api-key/"

/** The scrobbling accounts and servers, and what each page says while it connects. It outlives the pages, so
 *  a sign-in started on one page finishes on another. */
internal class ScrobblingAccounts(private val connection: RufinConnection, private val preferences: PreferencesConnection,
    private val context: Context) {
    var state by mutableStateOf<ScrobblingPreferences?>(null)
        private set
    /** A page's status line while it connects, or why it could not: by page id. */
    val status = mutableStateMapOf<String, String>()
    var working by mutableStateOf<String?>(null)
        private set

    fun load() = run(null) { it.loadPreferences() }

    /** Runs `action` for the page `id`; it shows `progress` meanwhile, and a failure after. */
    fun run(id: String?, progress: String? = null, done: suspend (ScrobblingPreferences) -> Unit = {},
        action: suspend (ScrobblingOwner) -> ScrobblingPreferences) {
        connection.viewModelScope.launch {
            working = id
            if (id != null) { if (progress != null) status[id] = progress else status.remove(id) }
            try {
                val next = action(preferences.nativeScrobbling())
                state = next
                id?.let(status::remove)
                done(next)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) {
                val message = error.message?.let(::translate) ?: translate("Couldn't connect")
                if (id != null) status[id] = message else connection.showFeedback(message)
            } finally { working = null }
        }
    }

    /** Applies the switches `edit` sets. */
    fun save(edit: (ScrobblingPreferences) -> ScrobblingPreferences) = run(null) { owner ->
        withContext(Dispatchers.IO) { owner.save(edit(owner.preferences())) }
    }

    /** Opens the account's sign-in page in the browser and waits for it. */
    fun connectBrowser(id: String, request: ScrobblingConnection, opening: String, done: suspend (ScrobblingPreferences) -> Unit = {}) =
        run(id, opening, done) { owner ->
            val session = owner.connect(request)
            try {
                while (true) when (val event = withContext(Dispatchers.Default) { session.next() }) {
                    is ScrobblingConnectionEvent.OpenUrl -> {
                        val failure = try {
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(event.url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                            null
                        } catch (error: Exception) {
                            translate("Couldn't open the sign-in page: {error}").replace("{error}", error.message.orEmpty())
                        }
                        event.opened.use { it.complete(failure) }
                        failure?.let(::error)
                    }
                    is ScrobblingConnectionEvent.Connected -> break
                    is ScrobblingConnectionEvent.Failed -> error(event.v1)
                    ScrobblingConnectionEvent.TimedOut -> error(translate("Couldn't connect"))
                }
            } finally { session.destroy() }
            owner.preferences()
        }
}

private fun accountStatus(connected: Boolean, username: String) = when {
    !connected -> translate("Not connected")
    username.isBlank() -> translate("Connected")
    else -> username.trim()
}

private fun connectionSubtitle(connected: Boolean, username: String) = when {
    !connected -> translate("Not connected")
    username.isBlank() -> translate("Connected")
    else -> translate("Connected as {username}").replace("{username}", username.trim())
}

private fun protocolTitle(protocol: ScrobblingProtocol) = translate(when (protocol) {
    ScrobblingProtocol.LISTEN_BRAINZ -> "ListenBrainz API"
    ScrobblingProtocol.AUDIOSCROBBLER -> "Audioscrobbler API"
})

private fun lyricsProviderTitle(provider: ExternalLyricsProvider) = when (provider) {
    ExternalLyricsProvider.LRCLIB -> "LRCLIB"
    ExternalLyricsProvider.NETEASE -> "NetEase"
    ExternalLyricsProvider.GENIUS -> "Genius"
    ExternalLyricsProvider.SIMP_MUSIC -> "SimpMusic"
}

/** The remote folders Rufin reads besides its music sources, read again when `revision` changes. */
@Composable
private fun rememberRemoteFolders(model: RufinConnection, revision: Int): List<FileIntegration> {
    var folders by remember { mutableStateOf<List<FileIntegration>>(emptyList()) }
    LaunchedEffect(model.sourceOwner, model.sources, revision) {
        val owner = model.sourceOwner ?: return@LaunchedEffect
        try { folders = withContext(Dispatchers.IO) { owner.fileIntegrations().filter { !it.musicSource } } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { model.runAction { throw error } }
    }
    return folders
}

/** Integrations: where Rufin scrobbles, looks covers and lyrics up, links to and reads remote folders. Each
 *  opens its own page. */
@Composable
internal fun IntegrationsPage(model: RufinConnection, preferences: PreferencesConnection, player: PlayerConnection,
    open: (IntegrationPage) -> Unit) {
    val accounts = preferences.accounts
    LaunchedEffect(preferences) { accounts.load() }
    val application = preferences.applicationSettings
    val lyrics = player.settings?.lyrics
    val folders = rememberRemoteFolders(model, 0)
    SettingsPage {
        if (application == null || lyrics == null) Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        else IntegrationList(accounts.state, application, lyrics, folders, open)
    }
}

@Composable
private fun IntegrationList(state: ScrobblingPreferences?, application: io.github.screwys.rufin.shared.rufin_core.ApplicationPreferences,
    lyrics: io.github.screwys.rufin.shared.lyrics.UserPreferences, folders: List<FileIntegration>, open: (IntegrationPage) -> Unit) {
    SettingsGroup(translate("Scrobbling")) {
        state?.let { current ->
            IntegrationRow(IntegrationPage("lastfm", "Last.fm"),
                accountStatus(current.lastfm.connected, current.lastfm.username), current.lastfm.connected && current.lastfm.enabled, open)
            val listenbrainz = current.listenbrainz.connected
            IntegrationRow(IntegrationPage("listenbrainz", "ListenBrainz"),
                accountStatus(listenbrainz, current.listenbrainz.username), listenbrainz && current.listenbrainz.enabled, open)
            IntegrationRow(IntegrationPage("librefm", "Libre.fm"),
                accountStatus(current.librefm.connected, current.librefm.username), current.librefm.connected && current.librefm.enabled, open)
            current.servers.forEach { server -> key(server.id) {
                IntegrationRow(IntegrationPage(SERVER + server.id, server.name),
                    "${accountStatus(server.connected, server.username)} · ${protocolTitle(server.protocol)}", server.connected && server.enabled, open)
            } }
        }
        IntegrationRow(IntegrationPage("add_server", translate("Add server")), translate("ListenBrainz or Audioscrobbler API"), false, open)
    }
    SettingsGroup(translate("Lookups")) {
        val metadata = application.externalMetadataEnabled
        IntegrationRow(IntegrationPage("metadata", translate("Metadata")),
            translate(if (metadata) "Covers, artist images, tags" else "Off"), metadata, open)
        IntegrationRow(IntegrationPage("lyrics", translate("Lyrics")),
            if (lyrics.externalLyricsEnabled) lyrics.externalLyricsProviders.joinToString(", ", transform = ::lyricsProviderTitle) else translate("Off"),
            lyrics.externalLyricsEnabled, open)
    }
    SettingsGroup(translate("Links")) {
        val links = application.externalSiteLinks
        val services = listOfNotNull("Last.fm".takeIf { links.lastfm }, "MusicBrainz".takeIf { links.musicbrainz },
            translate("Server").takeIf { links.server })
        IntegrationRow(IntegrationPage("links", translate("Page links")),
            if (links.enabled) services.joinToString(", ") else translate("Off"), links.enabled, open)
    }
    SettingsGroup(translate("Remote folders")) {
        for ((kind, name) in listOf("webdav" to translate("WebDAV / Nextcloud"), "smb" to translate("SMB / Samba"))) {
            val count = folders.count { (it.kind == "smb") == (kind == "smb") }
            IntegrationRow(IntegrationPage(kind, name), folderCountText(count.toULong()), count > 0, open)
        }
    }
}

@Composable
private fun IntegrationRow(page: IntegrationPage, status: String, active: Boolean, open: (IntegrationPage) -> Unit) {
    SettingsRow(page.name, summary = status, trailing = {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            if (active) Box(Modifier.size(8.dp).background(MaterialTheme.colorScheme.primary, CircleShape)
                .semantics { contentDescription = translate("Active") })
            RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp))
        }
    }, modifier = Modifier.clickable { open(page) })
}

/** The top of an integration's page: what it does or how it stands, and its action. */
@Composable
private fun IntegrationHero(status: String, action: String? = null, primary: Boolean = false, enabled: Boolean = true,
    act: () -> Unit = {}) {
    Surface(Modifier.fillMaxWidth(), RoundedCornerShape(18.dp), MaterialTheme.colorScheme.surfaceContainer) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            Text(status, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            action?.let { label ->
                if (primary) Button(act, enabled = enabled) { Text(label) }
                else FilledTonalButton(act, enabled = enabled) { Text(label) }
            }
        }
    }
}

@Composable
private fun LinkRow(preferences: PreferencesConnection, title: String, summary: String?, url: String) {
    val context = LocalContext.current
    SettingsRow(title, summary = summary, trailing = { RufinIcon("rufin-external-link-symbolic", null, Modifier.size(20.dp)) },
        modifier = Modifier.clickable {
            try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
            catch (error: Exception) { preferences.showFeedback(error.message ?: translate("Could not open browser")) }
        })
}

@Composable
private fun ScrobblingSwitches(accounts: ScrobblingAccounts, enabled: Boolean, nowPlaying: Boolean, description: String? = null,
    update: (ScrobblingPreferences, Boolean, Boolean) -> ScrobblingPreferences) {
    SettingsGroup(translate("Scrobbling"), description) {
        SettingsSwitch(translate("Scrobble plays"), enabled) { value -> accounts.save { update(it, value, nowPlaying) } }
        SettingsSwitch(translate("Now playing updates"), nowPlaying) { value -> accounts.save { update(it, enabled, value) } }
    }
}

/** Providers asked in order: the switched-on ones first, in their order, then the others. */
@Composable
private fun <T : Enum<T>> ProviderRows(all: List<T>, shown: List<T>, enabled: Boolean, describe: (T) -> Pair<String, String>,
    write: (List<T>) -> Unit) {
    // Off with its lookup, the list dims as the switch rows beside it do.
    Box(Modifier.alpha(if (enabled) 1f else .38f)) {
        SettingsReorderRows((shown + all.filter { it !in shown }).map { provider ->
            val (title, subtitle) = describe(provider)
            SettingsReorderItem(provider.name, title, provider in shown, enabled && provider in shown, enabled, subtitle.ifEmpty { null })
        }, onReorder = { next, _, _, _ -> write(next.mapNotNull { id -> shown.find { it.name == id } }) },
            onToggle = { id, on -> all.first { it.name == id }.let { provider -> write(if (on) shown + provider else shown - provider) } })
    }
}

/** An integration's page, under a bar with its name and the way back. */
@Composable
internal fun IntegrationDetailPage(model: RufinConnection, preferences: PreferencesConnection, player: PlayerConnection,
    page: IntegrationPage, open: (IntegrationPage) -> Unit, back: () -> Unit) {
    val accounts = preferences.accounts
    LaunchedEffect(preferences) { if (accounts.state == null) accounts.load() }
    SettingsPage {
        when {
            page.id == "metadata" -> MetadataPage(preferences, player)
            page.id == "lyrics" -> LyricsPage(player)
            page.id == "links" -> LinksPage(preferences)
            page.id == "webdav" || page.id == "smb" -> RemoteFoldersPage(model, page.id)
            page.id == "add_server" -> AddServerPage(accounts, open)
            else -> accounts.state?.let { state -> when {
                page.id == "lastfm" -> LastFmPage(preferences, player, accounts, state)
                page.id == "listenbrainz" -> ListenBrainzPage(preferences, accounts, state)
                page.id == "librefm" -> LibreFmPage(accounts, state)
                else -> state.servers.find { SERVER + it.id == page.id }?.let { ServerPage(accounts, state, it.id, page.id) }
                    // A removed server's page leaves.
                    ?: LaunchedEffect(Unit) { back() }
            } } ?: Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        }
    }
}

@Composable
private fun AccountHero(accounts: ScrobblingAccounts, id: String, connected: Boolean, username: String, suffix: String = "",
    connect: () -> Unit) {
    IntegrationHero(accounts.status[id] ?: (connectionSubtitle(connected, username) + suffix),
        translate(if (connected) "Reconnect" else "Connect"), primary = !connected, enabled = accounts.working == null, act = connect)
}

@Composable
private fun LastFmPage(preferences: PreferencesConnection, player: PlayerConnection, accounts: ScrobblingAccounts, state: ScrobblingPreferences) {
    val lastfm = state.lastfm
    AccountHero(accounts, "lastfm", lastfm.connected, lastfm.username) {
        accounts.connectBrowser("lastfm", ScrobblingConnection.LastFm(lastfm.apiKey, lastfm.apiSecret),
            translate("Opening Last.fm authorization...")) { withContext(Dispatchers.IO) { player.bridge?.retryExternalArtwork() } }
    }
    ScrobblingSwitches(accounts, lastfm.enabled, lastfm.nowPlayingEnabled) { current, enabled, nowPlaying ->
        current.copy(lastfm = current.lastfm.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
    }
    SettingsGroup(translate("Your own API key"), translate("Optional. Rufin has its own.")) {
        SettingsText(translate("API key"), lastfm.apiKey, secretSummary(lastfm.apiKey, translate("Not set")), secret = true) { value ->
            accounts.run("lastfm") { it.saveCredential(io.github.screwys.rufin.shared.rufin_core.ScrobblingCredentialEdit.LastFm(value.trim(), null)) }
        }
        SettingsText(translate("Shared secret"), lastfm.apiSecret, secretSummary(lastfm.apiSecret, translate("Not set")), secret = true) { value ->
            accounts.run("lastfm") { it.saveCredential(io.github.screwys.rufin.shared.rufin_core.ScrobblingCredentialEdit.LastFm(null, value.trim())) }
        }
        LinkRow(preferences, translate("Create API keys"), null, LASTFM_API_CREATE_URL)
    }
}

@Composable
private fun ListenBrainzPage(preferences: PreferencesConnection, accounts: ScrobblingAccounts, state: ScrobblingPreferences) {
    val listenbrainz = state.listenbrainz
    val token = listenbrainz.userToken
    val editing = remember { mutableStateOf(false) }
    // A token is checked with the server as it is entered.
    fun connect(value: String) = accounts.run("listenbrainz", translate("Checking the user token...")) { it.connectListenbrainz(value.trim()) }
    AccountHero(accounts, "listenbrainz", listenbrainz.connected, listenbrainz.username) {
        if (token.isBlank()) editing.value = true else connect(token)
    }
    SettingsGroup(translate("Account")) {
        SettingsText(translate("User token"), token, secretSummary(token, translate("Not set")), secret = true, editing = editing) { value ->
            if (value.isBlank()) accounts.status["listenbrainz"] = translate("Enter your user token first") else connect(value)
        }
        LinkRow(preferences, translate("Find your token"), null, LISTENBRAINZ_TOKEN_URL)
    }
    ScrobblingSwitches(accounts, listenbrainz.enabled, listenbrainz.nowPlayingEnabled) { current, enabled, nowPlaying ->
        current.copy(listenbrainz = current.listenbrainz.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
    }
}

@Composable
private fun LibreFmPage(accounts: ScrobblingAccounts, state: ScrobblingPreferences) {
    val librefm = state.librefm
    AccountHero(accounts, "librefm", librefm.connected, librefm.username) {
        accounts.connectBrowser("librefm", ScrobblingConnection.LibreFm, translate("Opening Libre.fm authorization..."))
    }
    ScrobblingSwitches(accounts, librefm.enabled, librefm.nowPlayingEnabled,
        translate("If the page doesn't load, then Libre.fm blocks your IP range/VPN")) { current, enabled, nowPlaying ->
        current.copy(librefm = current.librefm.copy(enabled = enabled, nowPlayingEnabled = nowPlaying))
    }
}

@Composable
private fun ServerPage(accounts: ScrobblingAccounts, state: ScrobblingPreferences, id: String, page: String) {
    val server = state.servers.first { it.id == id }
    val listenbrainz = server.protocol == ScrobblingProtocol.LISTEN_BRAINZ
    val editing = remember { mutableStateOf(false) }
    var username by remember(server.username) { mutableStateOf(server.username) }
    // The password is sent once for a session and not kept.
    var password by remember { mutableStateOf("") }
    val subtitle = connectionSubtitle(server.connected, server.username) + " · ${protocolTitle(server.protocol)}"
    // An Audioscrobbler server signs in again with its username and password once its session stops working.
    if (listenbrainz) AccountHero(accounts, page, server.connected, server.username, " · ${protocolTitle(server.protocol)}") {
        editing.value = true
    } else IntegrationHero(accounts.status[page] ?: subtitle, translate("Sign in again").takeUnless { server.connected }, primary = true,
        enabled = accounts.working == null) {
        accounts.run(page, translate("Signing in..."), done = { password = "" }) { it.signInServer(id, username.trim(), password) }
    }
    SettingsGroup(translate("Server")) {
        SettingsRow(translate("Protocol"), summary = protocolTitle(server.protocol))
        SettingsRow(translate("Server URL"), summary = server.url)
        if (listenbrainz) SettingsText(translate("User token"), "", secretSummary(if (server.connected) "set" else "", translate("Not set")),
            secret = true, editing = editing) { value ->
            accounts.run(page, translate("Checking the user token...")) { it.signInServer(id, "", value.trim()) }
        }
        else if (!server.connected) {
            SettingsText(translate("Username"), username, username.ifBlank { translate("Not set") }) { username = it }
            SettingsText(translate("Password"), password, secretSummary(password, translate("Not set")), secret = true) { password = it }
        }
    }
    ScrobblingSwitches(accounts, server.enabled, server.nowPlayingEnabled) { current, enabled, nowPlaying ->
        current.copy(servers = current.servers.map { if (it.id == id) it.copy(enabled = enabled, nowPlayingEnabled = nowPlaying) else it })
    }
    SettingsGroup {
        SettingsRow(translate("Remove server"), titleColor = MaterialTheme.colorScheme.error,
            modifier = Modifier.clickable(enabled = accounts.working == null) { accounts.run(page) { it.removeServer(id) } })
    }
}

@Composable
private fun AddServerPage(accounts: ScrobblingAccounts, open: (IntegrationPage) -> Unit) {
    var protocol by rememberSaveable { mutableStateOf(ScrobblingProtocol.LISTEN_BRAINZ) }
    var name by rememberSaveable { mutableStateOf("") }
    var url by rememberSaveable { mutableStateOf("") }
    var token by rememberSaveable { mutableStateOf("") }
    var username by rememberSaveable { mutableStateOf("") }
    // The password is sent once for a session and not kept.
    var password by remember { mutableStateOf("") }
    val listenbrainz = protocol == ScrobblingProtocol.LISTEN_BRAINZ
    IntegrationHero(accounts.status["add_server"] ?: translate("Self-hosted scrobbling"))
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SegmentedPill(listOf(ScrobblingProtocol.LISTEN_BRAINZ, ScrobblingProtocol.AUDIOSCROBBLER), protocol, { protocol = it },
            Modifier.fillMaxWidth(), equal = true) { option, _ ->
            Text(protocolTitle(option), style = MaterialTheme.typography.titleSmall, maxLines = 1)
        }
        Text(translate(if (listenbrainz) "Koito, Maloja and other ListenBrainz servers."
            else "GNU FM, Maloja and other Last.fm-style servers. On Maloja, the password is an API key."),
            Modifier.padding(horizontal = 12.dp), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    SettingsGroup {
        SettingsText(translate("Name"), name, name.ifBlank { translate("Not set") }) { name = it }
        SettingsText(translate("Server URL"), url, url.ifBlank { translate("Not set") }) { url = it }
        if (listenbrainz) SettingsText(translate("User token"), token, secretSummary(token, translate("Not set")), secret = true) { token = it }
        else {
            SettingsText(translate("Username"), username, username.ifBlank { translate("Not set") }) { username = it }
            SettingsText(translate("Password"), password, secretSummary(password, translate("Not set")), secret = true) { password = it }
        }
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Button({
            if (name.isBlank() || url.isBlank()) accounts.status["add_server"] = translate("Enter a name and URL.")
            else {
                val request = ScrobblingServerRequest(name.trim(), protocol, url.trim(), if (listenbrainz) "" else username.trim(),
                    if (listenbrainz) token.trim() else password)
                password = ""
                accounts.run("add_server", translate("Signing in..."), done = { next ->
                    name = ""; url = ""; token = ""; username = ""
                    val server = next.servers.last()
                    open(IntegrationPage(SERVER + server.id, server.name))
                }) { it.addServer(request) }
            }
        }, enabled = accounts.working == null) { Text(translate("Connect")) }
    }
}

/** Saves a lookup setting, then asks again for the artwork the lookups did not find. */
private fun lookup(preferences: PreferencesConnection, player: PlayerConnection, edit: SettingsOwner.() -> Unit) = preferences.changePreference {
    edit()
    player.bridge?.retryExternalArtwork()
}

@Composable
private fun MetadataPage(preferences: PreferencesConnection, player: PlayerConnection) {
    val application = preferences.applicationSettings ?: return
    val enabled = application.externalMetadataEnabled
    IntegrationHero(translate("Covers and artist images your library doesn't have"))
    SettingsGroup {
        SettingsSwitch(translate("External metadata lookup"), enabled, translate("Also album types and tag suggestions")) {
            lookup(preferences, player) { setExternalMetadataEnabled(it) }
        }
    }
    SettingsGroup(translate("Album covers"), translate("Asked in this order.")) {
        ProviderRows(CoverProvider.entries, application.coverProviders, enabled, { provider -> when (provider) {
            CoverProvider.MUSIC_BRAINZ -> "MusicBrainz" to ""
            CoverProvider.LAST_FM -> "Last.fm" to ""
        } }) { lookup(preferences, player) { setCoverProviders(it) } }
    }
    SettingsGroup(translate("Artist images"), translate("Asked in this order.")) {
        ProviderRows(ArtistImageProvider.entries, application.artistImageProviders, enabled, { provider -> when (provider) {
            ArtistImageProvider.FANART_TV -> "fanart.tv" to ""
            ArtistImageProvider.WIKIMEDIA_COMMONS -> "Wikimedia Commons" to ""
        } }) { lookup(preferences, player) { setArtistImageProviders(it) } }
    }
    SettingsGroup(translate("fanart.tv key"), translate("Optional. New images show up sooner with your own key.")) {
        SettingsText(translate("Personal API key"), application.fanartApiKey, secretSummary(application.fanartApiKey, translate("Not set")), secret = true) { value ->
            lookup(preferences, player) { setFanartApiKey(value.trim()) }
        }
        LinkRow(preferences, translate("Get a personal key"), null, FANART_KEY_URL)
    }
}

@Composable
private fun LyricsPage(player: PlayerConnection) {
    val lyrics = player.settings?.lyrics ?: return
    val enabled = lyrics.externalLyricsEnabled
    IntegrationHero(translate("Lyrics your library doesn't have"))
    SettingsGroup {
        SettingsSwitch(translate("External lyrics lookup"), enabled) { player.lyricsPreference { setExternalLyricsEnabled(it) } }
        SettingsSwitch(translate("Prefer server lyrics"), lyrics.preferServerLyrics, enabled = enabled) { player.lyricsPreference { setPreferServerLyrics(it) } }
    }
    SettingsGroup(translate("Providers"), translate("The closest match wins; on a tie, the higher one.")) {
        ProviderRows(ExternalLyricsProvider.entries, lyrics.externalLyricsProviders, enabled, { provider ->
            lyricsProviderTitle(provider) to translate(when (provider) {
                ExternalLyricsProvider.LRCLIB, ExternalLyricsProvider.SIMP_MUSIC -> "Synced and plain lyrics"
                ExternalLyricsProvider.NETEASE -> "Synced lyrics, some word by word"
                ExternalLyricsProvider.GENIUS -> "Plain lyrics"
            })
        }) { player.lyricsPreference { setExternalLyricsProviders(it) } }
    }
}

@Composable
private fun LinksPage(preferences: PreferencesConnection) {
    val links = preferences.applicationSettings?.externalSiteLinks ?: return
    fun change(edit: (io.github.screwys.rufin.shared.rufin_core.ExternalSiteLinkSettings) -> io.github.screwys.rufin.shared.rufin_core.ExternalSiteLinkSettings) =
        preferences.changePreference { setExternalSiteLinks(edit(applicationPreferences().externalSiteLinks)) }
    IntegrationHero(translate("Service icons on album and artist pages"))
    SettingsGroup {
        SettingsSwitch(translate("Show external site links"), links.enabled) { on -> change { it.copy(enabled = on) } }
    }
    SettingsGroup(translate("Services")) {
        SettingsSwitch("Last.fm", links.lastfm, enabled = links.enabled) { on -> change { it.copy(lastfm = on) } }
        SettingsSwitch("MusicBrainz", links.musicbrainz, enabled = links.enabled) { on -> change { it.copy(musicbrainz = on) } }
        SettingsSwitch(translate("Server"), links.server, enabled = links.enabled) { on -> change { it.copy(server = on) } }
    }
}

@Composable
private fun RemoteFoldersPage(model: RufinConnection, kind: String) {
    var revision by remember { mutableIntStateOf(0) }
    var editing by remember { mutableStateOf<String?>(null) }
    var adding by remember { mutableStateOf(false) }
    val folders = rememberRemoteFolders(model, revision).filter { (it.kind == "smb") == (kind == "smb") }
    IntegrationHero(translate(if (kind == "smb") "Remote folders on a Windows share or NAS" else "Remote folders over WebDAV"))
    SettingsGroup {
        folders.forEach { folder -> key(folder.id) {
            SettingsRow(folder.name, trailing = { RufinIcon("rufin-go-next-symbolic", null, Modifier.size(20.dp)) },
                modifier = Modifier.clickable { editing = folder.id })
        } }
        SettingsRow(translate(if (kind == "smb") "Add SMB folder" else "Add WebDAV folder"), titleColor = MaterialTheme.colorScheme.primary,
            leading = { RufinIcon("rufin-list-add-symbolic", null, Modifier.size(24.dp), MaterialTheme.colorScheme.primary) },
            modifier = Modifier.clickable { adding = true })
    }
    if (adding || editing != null) Dialog(onDismissRequest = { model.closeSourceSetup(); adding = false; editing = null },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        SourceSetupScreen(model, { adding = false; editing = null; revision++ }, editing, connection = kind)
    }
}
