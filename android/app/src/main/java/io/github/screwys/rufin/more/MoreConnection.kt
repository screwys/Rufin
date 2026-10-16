package io.github.screwys.rufin.more

import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.runtime.*
import io.github.screwys.rufin.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.Calendar
import java.util.Locale
import io.github.screwys.rufin.shared.rufin_core.*
import io.github.screwys.rufin.shared.library.ActivityOverview
import io.github.screwys.rufin.shared.library.CalendarActivityPeriod

internal enum class ReleaseCheck { Idle, Checking, Success, Error }

internal data class OverviewPeriod(val year: Int, val month: Int?) {
    val native get() = month?.let { CalendarActivityPeriod.Month(year, it.toUByte()) } ?: CalendarActivityPeriod.Year(year)
    val key get() = if (month == null) year.toString() else String.format(Locale.ROOT, "%04d-%02d", year, month)
}

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class MoreConnection(private val connection: RufinConnection, scope: CoroutineScope) {
    var bridge by mutableStateOf<AndroidMore?>(null)
        private set
    var releases by mutableStateOf<ReleaseHistory?>(null)
        private set
    var report by mutableStateOf<ActivityOverview?>(null)
        private set
    var releaseCheck by mutableStateOf(ReleaseCheck.Idle)
        private set
    var exporting by mutableStateOf(false)
        private set
    var reportError by mutableStateOf<String?>(null)
        private set
    var reportLoading by mutableStateOf(false)
        private set
    var preferences by mutableStateOf<ActivityOverviewSettings?>(null)
        private set
    var releaseError by mutableStateOf<String?>(null)
        private set
    private var releasesOwner by mutableStateOf<ReleaseUpdateOwner?>(null)
    private var settingsOwner: SettingsOwner? = null
    var reportPeriod by mutableStateOf<OverviewPeriod?>(null)
        private set
    val reportTitle get() = reportPeriod?.let { periodLabel(it.native) }.orEmpty()
    private val now = Calendar.getInstance()
    private val currentMonth = OverviewPeriod(now.get(Calendar.YEAR), now.get(Calendar.MONTH) + 1)
    var periods by mutableStateOf(listOf(currentMonth))
        private set
    var yearly by mutableStateOf(false)
        private set
    var overviewVisible by mutableStateOf(false)
    var selectedPeriod by mutableStateOf(currentMonth)
    var startupPeriod by mutableStateOf<OverviewPeriod?>(null)
        private set
    private var reportRevision by mutableIntStateOf(0)
    private var initializedPeriods = false
    private var overviewPrepared by mutableStateOf(false)
    private var autoplayPending = false
    private val preferenceEdits = Mutex()
    private var releaseCheckIntervalHours by mutableLongStateOf(6)

    init {
        scope.launch {
            snapshotFlow { connection.ready to connection.service }.collectLatest { (ready, service) ->
                bridge = null
                startupPeriod = null
                if (!ready || service == null) return@collectLatest
                val runtime = service.runtime.value?.getOrNull() ?: return@collectLatest
                val owner = runtime.more()
                val releaseOwner = runtime.releaseUpdates()
                val settings = runtime.settings()
                val changes = runtime.releaseEvents()
                releasesOwner = releaseOwner
                settingsOwner = settings
                try {
                    preferences = withContext(Dispatchers.IO) { settings.activityPreferences() }
                    releaseCheck = ReleaseCheck.Idle
                    bridge = owner
                    startupPeriod = when (val period = owner.startupActivityPeriod()) {
                        is CalendarActivityPeriod.Month -> OverviewPeriod(period.year, period.month.toInt())
                        is CalendarActivityPeriod.Year -> OverviewPeriod(period.v1, null)
                        else -> null
                    }
                    releaseOwner.check()
                    while (isActive) {
                        val next = withContext(Dispatchers.Default) { changes.next() }
                        when (next) {
                            is ReleaseUpdate.Refreshed -> {
                                releases = next.history
                                releaseError = null
                                next.notificationVersion?.let { version ->
                                    connection.showFeedback(translate("New release is available"))
                                    withContext(Dispatchers.IO) { releaseOwner.markSeen(version) }
                                }
                            }
                            is ReleaseUpdate.Failed -> releaseError = next.error
                            is ReleaseUpdate.Updated -> releases = releases?.copy(installedVersion = next.version, availableVersion = null)
                            else -> Unit
                        }
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
                finally { bridge = null; releasesOwner = null; settingsOwner = null; changes.destroy(); owner.destroy(); releaseOwner.destroy(); settings.destroy() }
            }
        }
        scope.launch {
            snapshotFlow { releasesOwner to releaseCheckIntervalHours }.collectLatest { (owner, hours) ->
                if (owner == null) return@collectLatest
                while (isActive) {
                    delay(hours * 60 * 60 * 1000)
                    owner.check()
                }
            }
        }
        scope.launch {
            snapshotFlow { Triple(bridge, selectedPeriod, reportRevision) to (overviewVisible && overviewPrepared) }.collectLatest { (request, visible) ->
                val (owner, period) = request
                if (owner == null || !visible) return@collectLatest
                val autoplay = autoplayPending
                autoplayPending = false
                reportLoading = true
                reportError = null
                try {
                    val loaded = withContext(Dispatchers.Default) { owner.activityOverview(period.year, period.month?.toUByte()) }
                    report = loaded
                    reportPeriod = period
                    if (autoplay && preferences?.autoplayFirstTrack != false) {
                        loaded.tracks.firstOrNull()?.let { connection.openOverview(it.mediaUri, it.metadata.title, "track", period.key) }
                    }
                }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) {
                    reportError = error.message ?: translate("Could not load listening overview")
                    connection.runAction { throw error }
                }
                finally { reportLoading = false }
            }
        }
    }

    fun prepareOverview(autoplay: Boolean = true) {
        overviewPrepared = false
        reportLoading = true
        reportError = null
        connection.runAction {
            try {
                val owner = bridge ?: return@runAction
                val keys = owner.activityMonths().filter { it.matches(Regex("[0-9]{4}-[0-9]{2}")) }.toSet()
                val earliest = keys.minOrNull()?.let { OverviewPeriod(it.substring(0, 4).toInt(), it.substring(5).toInt()) } ?: currentMonth
                val months = mutableListOf<OverviewPeriod>()
                var year = currentMonth.year
                var month = currentMonth.month!!
                while (year > earliest.year || (year == earliest.year && month >= earliest.month!!)) {
                    months += OverviewPeriod(year, month)
                    if (month == 1) { year--; month = 12 } else month--
                }
                periods = if (yearly) months.map { OverviewPeriod(it.year, null) }.distinct() else months
                if (!initializedPeriods) {
                    val previous = months.getOrNull(1)
                    selectedPeriod = if (previous != null && previous.key in keys) previous else currentMonth
                    initializedPeriods = true
                }
                if (yearly) selectedPeriod = OverviewPeriod(selectedPeriod.year, null)
                else if (selectedPeriod.month == null) selectedPeriod = months.firstOrNull { it.year == selectedPeriod.year } ?: currentMonth
                autoplayPending = autoplay
                overviewPrepared = true
                reportRevision++
            } catch (cancelled: CancellationException) { reportLoading = false; throw cancelled }
            catch (error: Exception) { reportLoading = false; reportError = error.message ?: translate("Could not load listening overview"); throw error }
        }
    }

    fun openStartupOverview() {
        val period = startupPeriod ?: return
        startupPeriod = null
        selectedPeriod = period
        yearly = period.month == null
        initializedPeriods = true
        connection.runAction { withContext(Dispatchers.IO) { settingsOwner?.markActivityOpened(period.native) } }
    }

    fun retryOverview() { if (overviewPrepared) reportRevision++ else prepareOverview(autoplay = false) }
    fun setReleaseCheckInterval(hours: Long) { releaseCheckIntervalHours = hours }
    fun chooseYearly(value: Boolean) { yearly = value; prepareOverview(autoplay = false) }
    fun movePeriod(older: Boolean) {
        val index = periods.indexOf(selectedPeriod)
        periods.getOrNull(index + if (older) 1 else -1)?.let { selectedPeriod = it }
    }
    fun choosePeriod(period: OverviewPeriod) { selectedPeriod = period }
    fun updatePreferences(update: (ActivityOverviewSettings) -> ActivityOverviewSettings) = connection.runAction {
        preferenceEdits.withLock {
            val owner = settingsOwner ?: return@withLock
            preferences = withContext(Dispatchers.IO) {
                owner.setActivityPreferences(update(owner.activityPreferences()))
                owner.activityPreferences()
            }
        }
    }
    fun checkReleases() {
        if (releaseCheck == ReleaseCheck.Checking || bridge == null) return
        releaseCheck = ReleaseCheck.Checking
        connection.runAction {
            try { releaseCheck = if (releasesOwner?.checkNow() == true) ReleaseCheck.Success else ReleaseCheck.Error }
            catch (cancelled: CancellationException) { releaseCheck = ReleaseCheck.Idle; throw cancelled }
            catch (error: Exception) { releaseCheck = ReleaseCheck.Error; throw error }
        }
    }
    fun exportOverview(dark: Boolean, foreground: Int, background: Int, accent: Int) {
        if (exporting || reportLoading || reportError != null || reportPeriod != selectedPeriod) return
        val owner = bridge ?: return
        val period = selectedPeriod
        exporting = true
        connection.runAction {
            try {
                val png = owner.exportOverviewPng(period.year, period.month?.toUByte(), dark,
                    foreground.toUInt(), background.toUInt(), accent.toUInt())
                connection.prepareOverviewExport(png, "rufin-${period.key}.png")
            } finally { exporting = false }
        }
    }
}
