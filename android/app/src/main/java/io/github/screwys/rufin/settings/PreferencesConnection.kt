package io.github.screwys.rufin.settings

import io.github.screwys.rufin.app.RufinConnection

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.provider.OpenableColumns
import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidPreferences
import io.github.screwys.rufin.core.AndroidPreferencesState
import io.github.screwys.rufin.core.installCatalogs
import io.github.screwys.rufin.browse.BrowseSwipeAction
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject

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
    var applicationSettings by mutableStateOf<JSONObject?>(null)
        private set
    val reduceMotion by derivedStateOf { applicationSettings?.optBoolean("reduce_motion") == true }
    val showDownloadedBadges by derivedStateOf { applicationSettings?.optBoolean("show_downloaded_badges", true) != false }

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
                installedCatalogIds = null
                bridge = preferences
                val subscription = preferences.subscribe(packaged.map { it.removeSuffix(".po") })
                try {
                    applicationSettings = JSONObject(preferences.applicationSettings())
                    connection.more.setReleaseCheckInterval(applicationSettings!!.getLong("release_check_interval_hours"))
                    while (currentCoroutineContext().isActive) {
                        val next = subscription.next()
                        applyLanguage(next.language)
                        state = next
                    }
                } finally {
                    bridge = null
                    subscription.destroy()
                    preferences.destroy()
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
            installCatalogs(catalogs.map { context.assets.open(it).bufferedReader().use { reader -> reader.readText() } })
        }
        installedCatalogIds = catalogs
        translationRevision += 1
    }

    fun selectTheme(id: String) = connection.runAction { bridge?.setTheme(id) }
    fun refreshApplicationSettings() = connection.runAction {
        bridge?.let {
            applicationSettings = JSONObject(it.applicationSettings())
            connection.more.setReleaseCheckInterval(applicationSettings!!.getLong("release_check_interval_hours"))
        }
    }
    fun changePreference(field: String, value: Any) = connection.runAction {
        val encoded = if (value is String) JSONObject.quote(value) else value.toString()
        bridge?.let {
            it.setApplicationPreference(field, encoded)
            applicationSettings = JSONObject(it.applicationSettings())
            connection.more.setReleaseCheckInterval(applicationSettings!!.getLong("release_check_interval_hours"))
        }
    }
    fun selectAccent(id: String) = connection.runAction { bridge?.setAccent(id) }
    suspend fun nativePreferences(): AndroidPreferences = checkNotNull(bridge)
    fun changeSecretStorage(mode: String) = connection.runAction {
        bridge?.let { it.setSecretStorage(mode); applicationSettings = JSONObject(it.applicationSettings()) }
    }
    fun selectThemeAccent(theme: String, id: String) = connection.runAction { bridge?.setThemeAccent(theme, id) }
    fun selectLanguage(id: String) = connection.runAction { bridge?.setLanguage(id) }
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
