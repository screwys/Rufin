package io.github.screwys.rufin.settings

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.sources.SourcesPage

@Composable
internal fun LibrarySettingsPage(model: RufinConnection, preferences: PreferencesConnection,
    addSource: () -> Unit, editSource: (String) -> Unit) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val state = preferences.applicationSettings
    if (state == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    SettingsPage {
        SourcesPage(model, addSource, editSource, embedded = true)
        SettingsGroup(translate("Downloads")) {
            SettingsSwitch(translate("Show downloaded badge"), state.optBoolean("show_downloaded_badges")) {
                preferences.changePreference("show_downloaded_badges", it)
            }
        }
    }
}
