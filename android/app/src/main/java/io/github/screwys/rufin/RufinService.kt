package io.github.screwys.rufin

import io.github.screwys.rufin.player.AndroidAudioFocus
import io.github.screwys.rufin.platform.AndroidLibraryObserver
import io.github.screwys.rufin.player.AndroidOutputRoutes
import io.github.screwys.rufin.platform.AndroidPermissions
import io.github.screwys.rufin.player.RufinPlayer

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.content.pm.PackageManager
import android.media.AudioManager
import android.net.wifi.WifiManager
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.DefaultMediaNotificationProvider
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import androidx.media3.session.MediaStyleNotificationHelper
import io.github.screwys.rufin.core.AndroidPlaybackState
import io.github.screwys.rufin.core.AndroidRepeatMode
import io.github.screwys.rufin.core.AndroidRuntime
import io.github.screwys.rufin.core.AndroidTransportState
import io.github.screwys.rufin.core.installCatalogs
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.collectLatest

@UnstableApi
class RufinService : MediaSessionService() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var runtimeJob: Job? = null
    private val mutableRuntime = MutableStateFlow<Result<AndroidRuntime>?>(null)
    val runtime = mutableRuntime.asStateFlow()
    private val mutablePlayback = MutableStateFlow<AndroidPlaybackState?>(null)
    val playback = mutablePlayback.asStateFlow()
    private val mutablePlaybackNotices = MutableSharedFlow<String>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)
    val playbackNotices = mutablePlaybackNotices.asSharedFlow()
    private lateinit var nativePlayer: RufinPlayer
    val player: Player get() = nativePlayer
    internal fun positionNow(): ULong = nativePlayer.positionNow()
    lateinit var outputs: AndroidOutputRoutes
        private set
    private lateinit var session: MediaSession
    private lateinit var audioFocus: AndroidAudioFocus
    private lateinit var wakeLock: PowerManager.WakeLock
    private var multicastLock: WifiManager.MulticastLock? = null
    private lateinit var activityIntent: PendingIntent
    private lateinit var libraryObserver: AndroidLibraryObserver
    private val libraryChanges = kotlinx.coroutines.channels.Channel<Unit>(kotlinx.coroutines.channels.Channel.CONFLATED)

    inner class Connection : Binder() { val service get() = this@RufinService }

    private val noisyReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY && playback.value?.localOutput == true) pause()
        }
    }

    override fun onCreate() {
        super.onCreate()
        nativePlayer = RufinPlayer(this)
        outputs = AndroidOutputRoutes(this)
        libraryObserver = AndroidLibraryObserver(this) { libraryChanges.trySend(Unit) }
        activityIntent = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        if (Build.VERSION.SDK_INT >= 26) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CHANNEL_PLAYBACK, getString(R.string.app_name), NotificationManager.IMPORTANCE_LOW))
        }
        setMediaNotificationProvider(DefaultMediaNotificationProvider.Builder(this)
            .setNotificationId(NOTIFICATION_PLAYBACK)
            .setChannelId(CHANNEL_PLAYBACK)
            .setChannelName(R.string.app_name)
            .build())
        session = MediaSession.Builder(this, nativePlayer).setSessionActivity(activityIntent).build()
        addSession(session)
        audioFocus = AndroidAudioFocus(this, ::playingLocally,
            pause = { runtime.value?.getOrNull()?.pause() },
            resume = {
                try { play() } catch (error: Exception) { nativePlayer.failed(error) }
            })
        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Rufin:playback")
        wakeLock.setReferenceCounted(false)
        multicastLock = applicationContext.getSystemService(WifiManager::class.java)
            ?.createMulticastLock("Rufin:nearby")?.apply { setReferenceCounted(false) }
        ContextCompat.registerReceiver(this, noisyReceiver, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
            ContextCompat.RECEIVER_NOT_EXPORTED)
        runtimeJob = scope.launch { runRuntime() }
    }

    private suspend fun runRuntime() = coroutineScope {
        var core: AndroidRuntime? = null
        try {
            // Connect can start discovery while the native runtime opens.
            multicastLock?.let { if (!it.isHeld) it.acquire() }
            withContext(Dispatchers.IO) {
                NativeHost.start(this@RufinService)
                val packaged = assets.list("").orEmpty().filter { it.endsWith(".po") }.toSet()
                val languages = resources.configuration.locales
                val catalogs = (0 until languages.size()).flatMap { index ->
                    val locale = languages[index]
                    listOf(locale.toLanguageTag().replace('-', '_'), locale.language)
                }.distinct().map { "$it.po" }.filter { it in packaged }
                installCatalogs(catalogs.map { assets.open(it).bufferedReader().use { reader -> reader.readText() } })
                core = AndroidRuntime(filesDir.absolutePath, cacheDir.absolutePath)
            }
            val started = checkNotNull(core)
            mutableRuntime.value = Result.success(started)
            launch {
                val artworkPlayer = started.player()
                try {
                    playback.map { state -> state?.let { current ->
                        current.artworkIdentity?.toList()?.let { it to current.artworkRevision }
                    } }.distinctUntilChanged().collectLatest { key ->
                        val (identity, revision) = key ?: return@collectLatest
                        try {
                            val artwork = artworkPlayer.artwork(identity.toByteArray(), 512u)
                            nativePlayer.updateArtwork(identity, revision, artwork?.bytes)
                        } catch (cancelled: CancellationException) {
                            throw cancelled
                        } catch (_: Exception) {
                            Log.w("Rufin", "Could not load media notification artwork")
                        }
                    }
                } finally { artworkPlayer.destroy() }
            }
            launch {
                val network = started.subscribeNetwork()
                try {
                    while (isActive) {
                        val nearby = network.nearbyDiscovery()
                        multicastLock?.let { lock ->
                            if (nearby && !lock.isHeld) lock.acquire()
                            if (!nearby && lock.isHeld) lock.release()
                        }
                    }
                } finally {
                    network.destroy()
                    multicastLock?.let { if (it.isHeld) it.release() }
                }
            }
            launch {
                val sources = started.subscribeSources()
                var selectedSource: String? = null
                try {
                    launch {
                        for (change in libraryChanges) {
                            val selected = selectedSource ?: continue
                            try { started.refreshSource(selected) }
                            catch (cancelled: CancellationException) { throw cancelled }
                            catch (error: Exception) { Log.w("Rufin", "Could not refresh changed documents", error) }
                        }
                    }
                    while (isActive) {
                        val state = sources.next()
                        selectedSource = state.selectedSourceId
                        libraryObserver.update(state.documentRoots.map { it.uri })
                    }
                } finally { sources.destroy() }
            }
            Log.i("Rufin", "Shared runtime started")
            val subscription = started.subscribePlayback()
            try {
                while (isActive) {
                    val state = subscription.next()
                    state.operationFailures.forEach { mutablePlaybackNotices.tryEmit(it) }
                    val previous = mutablePlayback.value
                    nativePlayer.update(state)
                    mutablePlayback.value = state
                    if (!state.localOutput || state.state == AndroidTransportState.STOPPED || state.state == AndroidTransportState.FAILED) {
                        audioFocus.abandon()
                    }
                    val active = state.desiredPlaying && (state.state == AndroidTransportState.PLAYING || state.state == AndroidTransportState.BUFFERING)
                    val startingLocalPlayback = active && state.localOutput &&
                        (previous?.desiredPlaying != true || !previous.localOutput ||
                            previous.state == AndroidTransportState.STOPPED || previous.state == AndroidTransportState.FAILED)
                    if (startingLocalPlayback && !audioFocus.hasFocus && !audioFocus.resumeOnGain) {
                        try {
                            preparePlayback()
                        } catch (error: Exception) {
                            started.pause()
                            nativePlayer.failed(error)
                        }
                    }
                    if (active && !wakeLock.isHeld) wakeLock.acquire()
                    if (!active && wakeLock.isHeld) wakeLock.release()
                }
            } finally {
                subscription.destroy()
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            Log.e("Rufin", "Could not run the shared runtime", error)
            mutableRuntime.value = Result.failure(error)
            nativePlayer.update(null)
            mutablePlayback.value = null
            nativePlayer.failed(error)
            audioFocus.abandon()
            if (wakeLock.isHeld) wakeLock.release()
            ServiceCompat.stopForeground(this@RufinService, ServiceCompat.STOP_FOREGROUND_REMOVE)
            stopSelf()
        } finally {
            multicastLock?.let { if (it.isHeld) it.release() }
            withContext(NonCancellable + Dispatchers.IO) { core?.destroy() }
        }

    }

    fun retryStartup() {
        if (runtime.value?.isFailure != true) return
        val previous = runtimeJob
        runtimeJob = scope.launch {
            previous?.cancelAndJoin()
            mutableRuntime.value = null
            runRuntime()
        }
    }

    override fun onBind(intent: Intent?): IBinder? {
        val mediaBinder = super.onBind(intent)
        return if (intent?.action == ACTION_BIND_RUNTIME) Connection() else mediaBinder
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession = session

    override fun onTaskRemoved(rootIntent: Intent?) {
        // Resolving or buffering a track still needs the playback service after the screen closes.
        if (isPlaybackOngoing && (playback.value?.desiredPlaying == true || audioFocus.resumeOnGain)) return
        super.onTaskRemoved(rootIntent)
    }

    private fun core(): AndroidRuntime = checkNotNull(runtime.value).getOrThrow()

    private fun playingLocally(): Boolean = playback.value?.let { it.localOutput && it.desiredPlaying } == true

    internal fun preparePlayback() {
        core()
        val alreadyOngoing = isPlaybackOngoing
        try {
            // Android 15 requires a foreground service before a background media control requests focus.
            val current = playback.value
            if (!alreadyOngoing) {
                val notification = NotificationCompat.Builder(this, CHANNEL_PLAYBACK)
                    .setSmallIcon(androidx.media3.session.R.drawable.media3_notification_small_icon)
                    .setContentTitle(current?.title?.takeIf { it.isNotEmpty() } ?: getString(R.string.app_name))
                    .setContentText(current?.artist)
                    .setContentIntent(activityIntent)
                    .setOngoing(true)
                    .setStyle(MediaStyleNotificationHelper.MediaStyle(session))
                    .build()
                ContextCompat.startForegroundService(this, Intent(this, RufinService::class.java))
                ServiceCompat.startForeground(this, NOTIFICATION_PLAYBACK, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
            }
            check(current?.localOutput == false || audioFocus.acquire()) { "Android did not grant audio focus" }
            nativePlayer.clearCommandError()
        } catch (error: Exception) {
            if (!alreadyOngoing) {
                ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
            throw error
        }
    }

    suspend fun openUris(uris: List<String>) = scope.async {
        if (uris.isEmpty()) return@async
        preparePlayback()
        try {
            core().openUris(uris)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            nativePlayer.failed(error)
            if (playback.value?.desiredPlaying != true) {
                audioFocus.abandon()
                ServiceCompat.stopForeground(this@RufinService, ServiceCompat.STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
            throw error
        }
    }.await()

    suspend fun addMediaLibrary() = scope.async {
        val permission = if (Build.VERSION.SDK_INT >= 33) android.Manifest.permission.READ_MEDIA_AUDIO
            else android.Manifest.permission.READ_EXTERNAL_STORAGE
        val alreadyGranted = ContextCompat.checkSelfPermission(this@RufinService, permission) == PackageManager.PERMISSION_GRANTED
        if (!alreadyGranted && !requestPermission(permission)) return@async false
        val started = runtime.filterNotNull().first().getOrThrow()
        val uri = android.provider.MediaStore.Audio.Media.EXTERNAL_CONTENT_URI.toString()
        if (!started.hasDocumentRoot(uri)) {
            started.addDocumentRoot(uri)
        } else if (!alreadyGranted) {
            val sources = started.subscribeSources()
            try {
                sources.next().sources.firstOrNull { it.kind == "local" }?.let { started.refreshSource(it.id) }
            } finally { sources.destroy() }
        }
        true
    }.await()

    internal suspend fun playRequest(action: suspend () -> Unit) {
        preparePlayback()
        try { action() }
        catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
        catch (error: Exception) {
            nativePlayer.failed(error)
            if (playback.value?.desiredPlaying != true) {
                audioFocus.abandon()
                ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
            throw error
        }
    }

    suspend fun requestLocalNetwork() = scope.async {
        if (requestLocalNetworkAccess()) {
            val sources = core().subscribeSources()
            try { sources.next().selectedSourceId?.let { core().refreshSource(it) } }
            finally { sources.destroy() }
        }
    }.await()

    suspend fun requestLocalNetworkAccess(): Boolean = scope.async {
        val permission = when {
            Build.VERSION.SDK_INT >= 37 -> android.Manifest.permission.ACCESS_LOCAL_NETWORK
            Build.VERSION.SDK_INT >= 36 -> android.Manifest.permission.NEARBY_WIFI_DEVICES
            else -> null
        }
        val granted = permission == null || requestPermission(permission)
        if (granted) multicastLock?.let { if (!it.isHeld) it.acquire() }
        granted
    }.await()

    private suspend fun requestPermission(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED ||
            withContext(Dispatchers.IO) { AndroidPermissions.request(permission) }

    fun play() {
        if (playback.value?.mediaUri != null) preparePlayback()
        core().play()
    }
    fun pause() { audioFocus.abandon(); core().pause() }
    fun stop() { audioFocus.abandon(); core().stop() }
    fun next() { preparePlayback(); core().next() }
    fun previous() = core().previous()
    fun seekMillis(millis: ULong) = core().seekMillis(millis)
    fun setVolume(volume: Double) = core().setVolume(volume)
    fun setShuffle(enabled: Boolean) = core().setShuffle(enabled)
    fun setRepeat(mode: AndroidRepeatMode) = core().setRepeat(mode)

    override fun onDestroy() {
        scope.cancel()
        unregisterReceiver(noisyReceiver)
        audioFocus.close()
        if (wakeLock.isHeld) wakeLock.release()
        multicastLock?.let { if (it.isHeld) it.release() }
        outputs.close()
        libraryObserver.close()
        libraryChanges.close()
        session.release()
        nativePlayer.release()
        super.onDestroy()
    }

    companion object {
        const val ACTION_BIND_RUNTIME = "io.github.screwys.rufin.BIND_RUNTIME"
        private const val CHANNEL_PLAYBACK = "playback"
        private const val NOTIFICATION_PLAYBACK = 1001
    }
}
