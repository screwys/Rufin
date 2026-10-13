package io.github.screwys.rufin.settings

import io.github.screwys.rufin.shared.rufin_core.ApplicationPreferences
import io.github.screwys.rufin.shared.rufin_core.SettingsOwner
import io.github.screwys.rufin.shared.rufin_core.ThemePreference
import io.github.screwys.rufin.shared.rufin_core.AccentPreference
import io.github.screwys.rufin.shared.rufin_core.ScrobblingOwner

import io.github.screwys.rufin.app.RufinConnection

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.provider.OpenableColumns
import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidPreferences
import io.github.screwys.rufin.core.AndroidPreferencesState
import io.github.screwys.rufin.core.installCatalogs
import io.github.screwys.rufin.core.installTranslations
import io.github.screwys.rufin.browse.BrowseSwipeAction
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class PreferencesConnection(private val connection: RufinConnection, context: Context) {
    private val context = context.applicationContext
    private val packaged = context.assets.list("").orEmpty().filter { it.endsWith(".po") }.toSet()
    var state by mutableStateOf<AndroidPreferencesState?>(null)
        private set
    var translationRevision by mutableIntStateOf(0)
        private set
    private var bridge: AndroidPreferences? = null
    private val catalogLock = Mutex()
    private var installedCatalogIds: List<String>? = null
    private var gestures: SharedPreferences? = null
    var swipeLeft by mutableStateOf(BrowseSwipeAction.Favorite)
        private set
    var swipeRight by mutableStateOf(BrowseSwipeAction.PlayLater)
        private set
    var themeErrors by mutableStateOf<List<String>>(emptyList())
        private set
    var applicationSettings by mutableStateOf<ApplicationPreferences?>(null)
        private set
    val reduceMotion by derivedStateOf { applicationSettings?.reduceMotion == true }
    val showDownloadedBadges by derivedStateOf { applicationSettings?.showDownloadedBadges != false }

    fun showThemeErrors(errors: List<String>) { themeErrors = errors }
    fun showFeedback(message: String) { connection.showFeedback(message) }

    suspend fun observe() {
        val savedGestures = withContext(Dispatchers.IO) {
            val storage = context.getSharedPreferences("appearance", Context.MODE_PRIVATE)
            val left = storage.getString("swipe_left", BrowseSwipeAction.Favorite.name)
            val right = storage.getString("swipe_right", BrowseSwipeAction.PlayLater.name)
            Triple(storage, left, right)
        }
        gestures = savedGestures.first
        swipeLeft = BrowseSwipeAction.entries.firstOrNull { it.name == savedGestures.second } ?: BrowseSwipeAction.Favorite
        swipeRight = BrowseSwipeAction.entries.firstOrNull { it.name == savedGestures.third } ?: BrowseSwipeAction.PlayLater
        snapshotFlow { connection.service }.collectLatest { service ->
            if (service == null) { state = null; return@collectLatest }
            service.runtime.collectLatest { result ->
                val runtime = result?.getOrNull() ?: return@collectLatest
                val preferences = runtime.preferences()
                settings = runtime.settings()
                scrobbling = runtime.scrobbling()
                installedCatalogIds = null
                bridge = preferences
                val subscription = preferences.subscribe(packaged.map { it.removeSuffix(".po") })
                try {
                    applicationSettings = settings!!.applicationPreferences()
                    connection.more.setReleaseCheckInterval(applicationSettings!!.releaseCheckIntervalHours.toLong())
                    while (currentCoroutineContext().isActive) {
                        val next = withContext(Dispatchers.Default) { subscription.next() }
                        applyLanguage(next.language)
                        state = next
                    }
                } finally {
                    bridge = null
                    subscription.destroy()
                    preferences.destroy()
                    settings?.destroy(); settings = null
                    scrobbling?.destroy(); scrobbling = null
                }
            }
        }
    }

    suspend fun refreshSystemLanguage() {
        applyLanguage("system", systemOnly = true)
    }

    private suspend fun applyLanguage(language: String, systemOnly: Boolean = false) = catalogLock.withLock {
        if (systemOnly && state?.language != "system") return@withLock
        val catalogs = run {
            val ids = if (language == "system") {
                val locales = context.resources.configuration.locales
                (0 until locales.size()).flatMap { index ->
                    val locale = locales[index]
                    listOf(locale.toLanguageTag().replace('-', '_'), locale.language)
                }
            } else {
                val id = language.substringBefore('.').substringBefore('@').replace('-', '_')
                listOf(id, id.substringBefore('_'))
            }
            ids.distinct().map { "$it.po" }.filter { it in packaged }
        }
        if (catalogs == installedCatalogIds) return@withLock
        withContext(Dispatchers.IO) {
            installTranslations(installCatalogs(catalogs.map {
                context.assets.open(it).bufferedReader().use { reader -> reader.readText() }
            }))
        }
        installedCatalogIds = catalogs
        translationRevision += 1
    }

    fun selectTheme(id: String) = connection.runAction { withContext(Dispatchers.IO) {
        settings?.setTheme(when (id) { "system" -> ThemePreference.System; "light" -> ThemePreference.Light; "dark" -> ThemePreference.Dark; else -> ThemePreference.Named(id) })
    } }
    private var settings: SettingsOwner? = null
    private var scrobbling: ScrobblingOwner? = null
    fun refreshApplicationSettings() = connection.runAction { refreshSettings() }
    private fun refreshSettings() {
        applicationSettings = settings?.applicationPreferences()
        applicationSettings?.let { connection.more.setReleaseCheckInterval(it.releaseCheckIntervalHours.toLong()) }
    }
    fun changePreference(edit: SettingsOwner.() -> Unit) = connection.runAction {
        withContext(Dispatchers.IO) { settings?.edit() }
        refreshSettings()
    }
    fun nativeSettings(): SettingsOwner = checkNotNull(settings)
    fun nativeScrobbling(): ScrobblingOwner = checkNotNull(scrobbling)
    fun selectAccent(id: String) = connection.runAction { withContext(Dispatchers.IO) { settings?.setAccent(AccentPreference.valueOf(id.uppercase())) } }
    fun changeSecretStorage(mode: String) = connection.runAction {
        val source = connection.connectedService().runtime.value!!.getOrThrow().source()
        try {
            source.changeSecretStorage(when (mode) {
                "system-keyring" -> io.github.screwys.rufin.shared.secrets.SecretStorageMode.SYSTEM_KEYRING
                "config-file" -> io.github.screwys.rufin.shared.secrets.SecretStorageMode.CONFIG_FILE
                else -> error("Unknown secret storage")
            })
            refreshSettings()
        } finally { source.destroy() }
    }
    fun selectThemeAccent(theme: String, id: String) = connection.runAction { withContext(Dispatchers.IO) { settings?.setThemeAccent(theme, id) } }
    fun selectLanguage(id: String) = connection.runAction { withContext(Dispatchers.IO) { settings?.setLanguage(id) } }
    fun selectSwipeAction(left: Boolean, id: String) = connection.runAction {
        val action = BrowseSwipeAction.valueOf(id)
        if (left) swipeLeft = action else swipeRight = action
        gestures?.edit()?.putString(if (left) "swipe_left" else "swipe_right", id)?.apply()
    }
    fun importTheme(uri: Uri) = connection.runAction {
        val (name, bytes) = withContext(Dispatchers.IO) {
            val name = context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) cursor.getString(0) else null
            } ?: uri.lastPathSegment ?: "custom.json"
            val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
                ?: error("Could not read theme")
            name to bytes
        }
        bridge?.importTheme(name, bytes)
    }
}
