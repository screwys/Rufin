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
    val settings = preferences.applicationSettings?.optJSONObject("activity_overview")
    if (settings == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    SettingsPage {
        listOf("monthly" to "Monthly", "yearly" to "Yearly").forEach { (period, label) ->
            val enabled = settings.optBoolean("${period}_enabled", true)
            SettingsGroup(translate(label)) {
                SettingsSwitch(translate("Open automatically"), enabled) { preferences.changePreference("activity_overview.${period}_enabled", it) }
                SettingsNumber(translate(if (period == "monthly") "Days before month ends" else "Days before year ends"),
                    settings.optDouble("${period}_days_before_end"), 0.0..31.0, enabled = enabled) {
                    preferences.changePreference("activity_overview.${period}_days_before_end", it)
                }
                SettingsNumber(translate(if (period == "monthly") "Days after month ends" else "Days after year ends"),
                    settings.optDouble("${period}_days_after_end"), 0.0..31.0, enabled = enabled) {
                    preferences.changePreference("activity_overview.${period}_days_after_end", it)
                }
            }
        }
    }
}
