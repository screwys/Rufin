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
import org.json.JSONArray
import org.json.JSONObject

internal enum class ReleaseCheck { Idle, Checking, Success, Error }

internal data class OverviewPeriod(val year: Int, val month: Int?) {
    val key get() = if (month == null) year.toString() else String.format(Locale.ROOT, "%04d-%02d", year, month)
}

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class MoreConnection(private val connection: RufinConnection, scope: CoroutineScope) {
    var bridge by mutableStateOf<AndroidMore?>(null)
        private set
    var releases by mutableStateOf<AndroidReleaseHistory?>(null)
        private set
    var report by mutableStateOf<AndroidActivityOverview?>(null)
        private set
    var releaseCheck by mutableStateOf(ReleaseCheck.Idle)
        private set
    var exporting by mutableStateOf(false)
        private set
    var reportError by mutableStateOf<String?>(null)
        private set
    var reportLoading by mutableStateOf(false)
        private set
    var preferences by mutableStateOf<JSONObject?>(null)
        private set
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
                val owner = service.runtime.value?.getOrNull()?.more() ?: return@collectLatest
                val changes = owner.subscribeReleases()
                try {
                    preferences = withContext(Dispatchers.IO) { JSONObject(owner.activityPreferences()) }
                    releaseCheck = ReleaseCheck.Idle
                    bridge = owner
                    startupPeriod = owner.startupActivityPeriod()?.let { OverviewPeriod(it.year, it.month?.toInt()) }
                    owner.checkReleasesAutomatically()
                    while (isActive) {
                        val next = changes.next()
                        releases = next
                        next.notificationVersion?.let { version ->
                            connection.showFeedback(translate("New release is available"))
                            owner.markReleaseSeen(version)
                        }
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
                finally { bridge = null; changes.destroy(); owner.destroy() }
            }
        }
        scope.launch {
            snapshotFlow { bridge to releaseCheckIntervalHours }.collectLatest { (owner, hours) ->
                if (owner == null) return@collectLatest
                while (isActive) {
                    delay(hours * 60 * 60 * 1000)
                    owner.checkReleasesAutomatically()
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
                    val loaded = owner.activityOverview(period.year, period.month?.toUByte())
                    report = loaded
                    if (autoplay && preferences?.optBoolean("autoplay_first_track", true) != false) {
                        loaded.tracks.firstOrNull()?.let { connection.openOverview(it, loaded.key) }
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
        connection.runAction { bridge?.markActivityOpened(period.year, period.month?.toUByte()) }
    }

    fun retryOverview() { if (overviewPrepared) reportRevision++ else prepareOverview(autoplay = false) }
    fun setReleaseCheckInterval(hours: Long) { releaseCheckIntervalHours = hours }
    fun chooseYearly(value: Boolean) { yearly = value; prepareOverview(autoplay = false) }
    fun movePeriod(older: Boolean) {
        val index = periods.indexOf(selectedPeriod)
        periods.getOrNull(index + if (older) 1 else -1)?.let { selectedPeriod = it }
    }
    fun choosePeriod(period: OverviewPeriod) { selectedPeriod = period }
    fun updatePreference(key: String, value: Any) = connection.runAction {
        preferenceEdits.withLock {
            val owner = bridge ?: return@withLock
            preferences = JSONObject(owner.setActivityPreference(key, JSONArray().put(value).toString().removeSurrounding("[", "]")))
        }
    }
    fun checkReleases() {
        if (releaseCheck == ReleaseCheck.Checking || bridge == null) return
        releaseCheck = ReleaseCheck.Checking
        connection.runAction {
            try { releaseCheck = if (bridge?.checkReleases() == true) ReleaseCheck.Success else ReleaseCheck.Error }
            catch (cancelled: CancellationException) { releaseCheck = ReleaseCheck.Idle; throw cancelled }
            catch (error: Exception) { releaseCheck = ReleaseCheck.Error; throw error }
        }
    }
    fun exportOverview(dark: Boolean, foreground: Int, background: Int, accent: Int) {
        if (exporting || reportLoading || reportError != null || report?.key != selectedPeriod.key) return
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
