package io.github.screwys.rufin.sources

import androidx.compose.runtime.*
import androidx.lifecycle.viewModelScope
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.sources.*
import kotlinx.coroutines.*
import io.github.screwys.rufin.ui.userMessage

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class SourceSetupConnection(private val model: RufinConnection, val sourceId: String?) {
    var form by mutableStateOf(SourceForm())
        private set
    private val scope = CoroutineScope(SupervisorJob(model.viewModelScope.coroutineContext[Job]) + Dispatchers.Main.immediate)
    private var bridge: SourceOwner? = null
    private var auth: SourceAuthorization? = null
    private var saved: SourceAuthorization? = null
    private var task: Job? = null
    private var plexLan = emptyList<DiscoveredServer>()
    private var discoverySubscription: SourceDiscoverySubscription? = null
    var closed = false
        private set
    var providers by mutableStateOf<List<Pair<String, String>>>(emptyList())
        private set
    var ready by mutableStateOf(false)
        private set
    var working by mutableStateOf(false)
        private set
    var failure by mutableStateOf<String?>(null)
        private set
    var discovery by mutableStateOf<DiscoveryUpdate?>(null)
        private set
    var choices by mutableStateOf<List<SourceChoice>>(emptyList())
        private set
    var profiles by mutableStateOf<List<SourceChoice>>(emptyList())
        private set
    var savedAccounts by mutableStateOf<List<SourceChoice>>(emptyList())
        private set
    var selectedServer by mutableStateOf(0u)
    var selectedProfile by mutableStateOf(0u)
    var homePin by mutableStateOf("")
    var code by mutableStateOf<String?>(null)
        private set
    var browserUrl by mutableStateOf<String?>(null)
        private set
    var authorized by mutableStateOf(false)
        private set
    var choosingProfiles by mutableStateOf(false)
        private set

    init {
        scope.launch {
            try {
                val service = model.connectedService()
                val runtime = service.runtime.value?.getOrThrow() ?: return@launch
                val setup = runtime.source()
                bridge = setup
                providers = listOf("local" to "Local", "jellyfin" to "Jellyfin", "emby" to "Emby", "plex" to "Plex", "navidrome" to "Navidrome", "subsonic" to "OpenSubsonic", "webdav" to "WebDAV", "smb" to "SMB / Samba").map { (kind, title) -> kind to translate(title) }
                sourceId?.let { form.load(withContext(Dispatchers.IO) { setup.editable(it) } ?: error(translate("Source is no longer available"))) }
                ready = true
                val subscription = setup.subscribeDiscovery()
                discoverySubscription = subscription
                launch {
                    try { while (isActive) {
                        val update = withContext(Dispatchers.Default) { subscription.next() }
                        discovery = update
                        if (update.provider == DiscoveryProvider.PLEX) plexLan = update.servers
                    } }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { failure = error.userMessage() }
                }
                if (form.kind == "plex" && sourceId == null) loadSavedAccounts()
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error.userMessage() }
        }
    }

    private fun run(block: suspend () -> Unit) {
        task?.cancel()
        task = scope.launch {
            working = true; failure = null
            try { block() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { failure = error.userMessage() }
            finally { working = false }
        }
    }
    fun reportError(error: Exception) { failure = error.userMessage() }
    fun setHalfStars(enabled: Boolean) = model.runAction {
        val id = sourceId ?: return@runAction
        val source = model.connectedService().runtime.value!!.getOrThrow().source()
        try { source.setHalfStars(id, enabled); form.halfStars = enabled }
        finally { source.destroy() }
    }
    fun selectProvider(kind: String) {
        cancelAuthorization()
        form = SourceForm().also { it.kind = kind }
        discovery = null
        if (kind == "plex") loadSavedAccounts()
    }
    private fun replaceAuthorization(session: SourceAuthorization) {
        auth?.let { it.cancel(); it.destroy() }
        auth = session
        code = null; browserUrl = null; authorized = false; choosingProfiles = false
        choices = emptyList(); profiles = emptyList(); selectedServer = 0u; selectedProfile = 0u
    }
    fun cancelAuthorization() {
        task?.cancel()
        auth?.let { it.cancel(); it.destroy() }
        auth = null
        code = null; browserUrl = null; authorized = false; choosingProfiles = false
        choices = emptyList(); profiles = emptyList(); working = false
    }
    private fun authorize(method: SourceAuthorizationMethod, openBrowser: (String) -> Unit) = run {
        val setup = bridge ?: return@run
        val session = withContext(Dispatchers.IO) { setup.authorize(method) }
        replaceAuthorization(session)
        while (true) when (val event = withContext(Dispatchers.Default) { session.next() }) {
            is SourceAuthorizationEvent.Code -> { code = event.code; browserUrl = event.url }
            is SourceAuthorizationEvent.OpenBrowser -> { browserUrl = event.url; openBrowser(event.url) }
            is SourceAuthorizationEvent.Ready -> {
                code = null
                if (event.kind == "plex") {
                    profiles = session.plexProfiles(null); choosingProfiles = true
                } else {
                    choices = withContext(Dispatchers.IO) { session.choices() }
                    session.fileSettings()?.let { form.loadFileSettings(it); form.secret = "" }
                    authorized = true
                }
                break
            }
        }
    }
    fun quickConnect(openBrowser: (String) -> Unit) = authorize(SourceAuthorizationMethod.Jellyfin(form.url, form.trustInvalidCertificate), openBrowser)
    fun embyLogin(pin: Boolean, openBrowser: (String) -> Unit) = authorize(SourceAuthorizationMethod.Emby(if (pin) EmbyConnectLoginMethod.Pin else EmbyConnectLoginMethod.Password(form.username, form.secret)), openBrowser)
    fun plexLogin(browser: Boolean, openBrowser: (String) -> Unit) = authorize(SourceAuthorizationMethod.Plex(
        if (browser) PlexLoginMethod.Browser else PlexLoginMethod.Password(form.username, form.secret,
            form.verification.trim().takeIf(String::isNotEmpty))), openBrowser)
    fun nextcloud(openBrowser: (String) -> Unit) {
        val settings: FileSourceSettings
        val credentials: FileCredentials
        try { settings = form.fileSettings(); credentials = form.fileCredentials() }
        catch (error: Exception) { failure = error.userMessage(); return }
        authorize(SourceAuthorizationMethod.Nextcloud(settings, credentials), openBrowser)
    }
    private fun loadSavedAccounts() = run {
        saved?.destroy()
        saved = bridge?.savedPlexLogins()
        savedAccounts = saved?.choices().orEmpty()
    }
    fun useSaved(index: UInt) = run {
        val session = saved ?: return@run
        saved = null
        replaceAuthorization(session)
        profiles = session.plexProfiles(index); choosingProfiles = true
    }
    fun choosePlexProfile() = run {
        choices = auth?.plexServers(selectedProfile, homePin.takeIf(String::isNotBlank), plexLan).orEmpty()
        choosingProfiles = false; authorized = true; selectedServer = 0u
    }
    fun changePlexProfile() { authorized = false; choosingProfiles = true; choices = emptyList() }
    suspend fun discover() {
        val provider = when (form.kind) { "jellyfin" -> DiscoveryProvider.JELLYFIN; "emby" -> DiscoveryProvider.EMBY; "plex" -> DiscoveryProvider.PLEX; else -> return }
        try {
            if (!model.connectedService().requestLocalNetworkAccess()) error(translate("Local network permission was denied"))
            bridge?.discoverServers(provider)
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error.userMessage() }
    }
    fun smbShares() = run {
        if (!model.connectedService().requestLocalNetworkAccess()) error(translate("Local network permission was denied"))
        choices = bridge?.smbShares(form.fileSettings(), form.fileCredentials()).orEmpty()
    }
    fun submit(onSuccess: () -> Unit) = run {
        val session = auth
        if (authorized && session != null) {
            val input = when (form.kind) {
                "jellyfin" -> SourceAuthorizationInput.Jellyfin(form.instantMix)
                "emby" -> SourceAuthorizationInput.Emby(selectedServer, form.instantMix)
                "plex" -> SourceAuthorizationInput.Plex(selectedServer, form.url.trim().takeIf(String::isNotEmpty), form.trustInvalidCertificate)
                "webdav" -> SourceAuthorizationInput.Files(form.fileSettings(), form.fileCredentialsEdit())
                else -> error(translate("Select Source"))
            }
            session.finish(form.name, sourceId, form.integration, input)
        } else form.submit(bridge ?: error(translate("Source setup is unavailable")), sourceId)
        if (sourceId != null) {
            form.secret = ""
            auth?.destroy(); auth = null; authorized = false
        }
        onSuccess()
    }
    fun music(onSuccess: () -> Unit) = run {
        if (!model.addMediaLibraryForSetup()) error(translate("Music access permission was denied"))
        onSuccess()
    }
    fun folder(uri: android.net.Uri, onSuccess: () -> Unit) = run { model.addFolderForSetup(uri); onSuccess() }
    fun close() {
        if (closed) return
        closed = true
        scope.cancel()
        auth?.let { it.cancel(); it.destroy() }
        saved?.destroy()
        discoverySubscription?.destroy()
        bridge?.destroy()
    }
}
