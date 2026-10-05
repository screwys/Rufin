package io.github.screwys.rufin.sources

import androidx.compose.runtime.*
import androidx.lifecycle.viewModelScope
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import io.github.screwys.rufin.ui.userMessage

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class SourceSetupConnection(private val model: RufinConnection, val sourceId: String?) {
    var form by mutableStateOf(SourceForm())
        private set
    private val scope = CoroutineScope(SupervisorJob(model.viewModelScope.coroutineContext[Job]) + Dispatchers.Main.immediate)
    private var bridge: AndroidSourceSetup? = null
    private var auth: AndroidSourceAuthorization? = null
    private var saved: AndroidSourceAuthorization? = null
    private var task: Job? = null
    private var discoverySubscription: AndroidSourceDiscoverySubscription? = null
    var closed = false
        private set
    var providers by mutableStateOf<List<AndroidSourceProvider>>(emptyList())
        private set
    var ready by mutableStateOf(false)
        private set
    var working by mutableStateOf(false)
        private set
    var failure by mutableStateOf<String?>(null)
        private set
    var discovery by mutableStateOf<AndroidSourceDiscovery?>(null)
        private set
    var choices by mutableStateOf<List<AndroidSourceChoice>>(emptyList())
        private set
    var profiles by mutableStateOf<List<AndroidSourceChoice>>(emptyList())
        private set
    var savedAccounts by mutableStateOf<List<AndroidSourceChoice>>(emptyList())
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
                val setup = runtime.sourceSetup()
                bridge = setup
                providers = withContext(Dispatchers.IO) { setup.providers() }
                sourceId?.let { form.load(setup.editable(it)) }
                ready = true
                val subscription = setup.subscribeDiscovery()
                discoverySubscription = subscription
                launch {
                    try { while (isActive) discovery = subscription.next() }
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
        val preferences = model.connectedService().runtime.value!!.getOrThrow().preferences()
        try { preferences.setHalfStars(id, enabled); form.halfStars = enabled }
        finally { preferences.destroy() }
    }
    fun selectProvider(kind: String) {
        cancelAuthorization()
        form = SourceForm().also { it.kind = kind }
        discovery = null
        if (kind == "plex") loadSavedAccounts()
    }
    private fun replaceAuthorization(session: AndroidSourceAuthorization) {
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
    private fun authorize(start: (AndroidSourceSetup) -> AndroidSourceAuthorization, openBrowser: (String) -> Unit) = run {
        val setup = bridge ?: return@run
        val session = withContext(Dispatchers.IO) { start(setup) }
        replaceAuthorization(session)
        while (true) when (val event = session.next()) {
            is AndroidSourceAuthorizationEvent.Code -> { code = event.code; browserUrl = event.url }
            is AndroidSourceAuthorizationEvent.OpenBrowser -> { browserUrl = event.url; openBrowser(event.url) }
            is AndroidSourceAuthorizationEvent.Ready -> {
                code = null
                if (event.kind == "plex") {
                    profiles = session.plexProfiles(null); choosingProfiles = true
                } else {
                    choices = withContext(Dispatchers.IO) { session.choices() }
                    session.fileSettings()?.let { form.loadFileSettings(org.json.JSONObject(it)); form.secret = "" }
                    authorized = true
                }
                break
            }
        }
    }
    fun quickConnect(openBrowser: (String) -> Unit) = authorize({ it.quickConnect(form.url, form.trustInvalidCertificate) }, openBrowser)
    fun embyLogin(pin: Boolean, openBrowser: (String) -> Unit) = authorize({ it.embyLogin(if (pin) null else form.username, if (pin) null else form.secret) }, openBrowser)
    fun plexLogin(browser: Boolean, openBrowser: (String) -> Unit) = authorize({
        it.plexLogin(if (browser) null else form.username, if (browser) null else form.secret, form.verification.trim().takeIf(String::isNotEmpty))
    }, openBrowser)
    fun nextcloud(openBrowser: (String) -> Unit) {
        val settings: String
        val credentials: String
        try { settings = form.fileSettings().toString(); credentials = form.fileCredentials().toString() }
        catch (error: Exception) { failure = error.userMessage(); return }
        authorize({ it.nextcloudLogin(settings, credentials) }, openBrowser)
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
        choices = auth?.plexServers(selectedProfile, homePin.takeIf(String::isNotBlank)).orEmpty()
        choosingProfiles = false; authorized = true; selectedServer = 0u
    }
    fun changePlexProfile() { authorized = false; choosingProfiles = true; choices = emptyList() }
    suspend fun discover() {
        val provider = when (form.kind) { "jellyfin" -> AndroidSourceDiscoveryProvider.JELLYFIN; "emby" -> AndroidSourceDiscoveryProvider.EMBY; "plex" -> AndroidSourceDiscoveryProvider.PLEX; else -> return }
        try {
            if (!model.connectedService().requestLocalNetworkAccess()) error(translate("Local network permission was denied"))
            bridge?.discover(provider)
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error.userMessage() }
    }
    fun smbShares() = run {
        if (!model.connectedService().requestLocalNetworkAccess()) error(translate("Local network permission was denied"))
        choices = bridge?.smbShares(form.fileSettings().toString(), form.fileCredentials().toString()).orEmpty()
    }
    fun submit(onSuccess: () -> Unit) = run {
        val session = auth
        if (authorized && session != null) session.finish(form.name, selectedServer, form.instantMix,
            form.url.trim().takeIf(String::isNotEmpty), form.trustInvalidCertificate, sourceId, form.integration,
            if (form.kind == "webdav") form.fileSettings().toString() else null,
            if (form.kind == "webdav") form.fileCredentials(editing = true).toString() else null)
        else (bridge ?: error(translate("Source setup is unavailable"))).submit(form.submission(sourceId), sourceId != null, form.integration)
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
