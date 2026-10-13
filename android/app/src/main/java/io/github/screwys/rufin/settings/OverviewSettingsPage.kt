package io.github.screwys.rufin.settings


import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.core.translate

@Composable
internal fun OverviewSettingsPage(preferences: PreferencesConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val settings = preferences.applicationSettings?.activityOverview
    if (settings == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    SettingsPage {
        listOf("monthly" to "Monthly", "yearly" to "Yearly").forEach { (period, label) ->
            val monthly = period == "monthly"
            val enabled = if (monthly) settings.monthlyEnabled else settings.yearlyEnabled
            SettingsGroup(translate(label)) {
                SettingsSwitch(translate("Open automatically"), enabled) { preferences.changePreference { setActivityPreferences(if (monthly) settings.copy(monthlyEnabled = it) else settings.copy(yearlyEnabled = it)) } }
                SettingsNumber(translate(if (period == "monthly") "Days before month ends" else "Days before year ends"),
                    (if (monthly) settings.monthlyDaysBeforeEnd else settings.yearlyDaysBeforeEnd).toDouble(), 0.0..31.0, enabled = enabled) {
                    preferences.changePreference { setActivityPreferences(if (monthly) settings.copy(monthlyDaysBeforeEnd = (it as Number).toLong().toUInt()) else settings.copy(yearlyDaysBeforeEnd = (it as Number).toLong().toUInt())) }
                }
                SettingsNumber(translate(if (period == "monthly") "Days after month ends" else "Days after year ends"),
                    (if (monthly) settings.monthlyDaysAfterEnd else settings.yearlyDaysAfterEnd).toDouble(), 0.0..31.0, enabled = enabled) {
                    preferences.changePreference { setActivityPreferences(if (monthly) settings.copy(monthlyDaysAfterEnd = (it as Number).toLong().toUInt()) else settings.copy(yearlyDaysAfterEnd = (it as Number).toLong().toUInt())) }
                }
            }
        }
    }
}
