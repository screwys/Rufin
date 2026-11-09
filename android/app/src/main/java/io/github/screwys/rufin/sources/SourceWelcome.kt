package io.github.screwys.rufin.sources

import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.R
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.SettingsSwitch
import io.github.screwys.rufin.shared.rufin_core.SourceOperation
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.ui.SourceIcon

/** What the first run's pages reach outside source setup. */
internal class SourceWelcome(val preferences: PreferencesConnection, val player: PlayerConnection,
    val importBackup: () -> Unit, val join: (String) -> Unit)

/** The source list: what was found first, then every kind. The first run opens on it with a greeting. */
@Composable
internal fun SourceChoices(model: RufinConnection, setup: SourceSetupConnection, welcome: SourceWelcome?, modifier: Modifier,
    added: () -> Unit, skip: () -> Unit) {
    val next: @Composable () -> Unit = { RufinIcon("rufin-go-next-symbolic", null) }
    LazyColumn(modifier, contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp)) {
        // The greeting, centred as GTK's: the icon and title lead, Skip stays a quiet corner button.
        if (welcome != null) item { Box(Modifier.fillMaxWidth()) {
            Column(Modifier.fillMaxWidth().padding(top = 28.dp, bottom = 8.dp), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Image(painterResource(R.drawable.app_icon), null, Modifier.padding(bottom = 4.dp).size(80.dp))
                Text(translate("Welcome to Rufin"), style = MaterialTheme.typography.headlineMedium, textAlign = TextAlign.Center)
                Text(translate("Choose where your music is. Add more places later in Settings."),
                    color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
            }
            TextButton(skip, Modifier.align(Alignment.TopEnd)) { Text(translate("Skip")) }
        } }
        item { SettingsGroup(translate("Found")) {
            SettingsRow(translate("Music on this phone"), summary = translate("Asks for music access"),
                leading = { RufinIcon("rufin-folder-music-symbolic", null) },
                trailing = { FilledTonalButton({ setup.music(added) }, enabled = !setup.working) { Text(translate("Add")) } })
            val found = setup.found
            if (found == null) SettingsRow(translate("Search this network"),
                summary = translate(if (welcome != null) "Servers and other Rufin devices" else "Jellyfin, Emby and Plex servers"),
                leading = { RufinIcon("rufin-network-server-symbolic", null) },
                trailing = { if (setup.finding) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) else next() },
                modifier = Modifier.clickable(enabled = !setup.finding, onClick = { setup.find(welcome != null) }))
            else {
                found.forEach { server ->
                    SettingsRow(server.name, summary = "${setup.providers.firstOrNull { it.first == server.kind }?.second ?: server.kind}, ${server.address}",
                        leading = { SourceIcon(server.kind, Modifier.size(24.dp)) }, trailing = next,
                        modifier = Modifier.clickable { setup.selectProvider(server.kind, server) })
                }
                if (welcome != null) model.connect.state?.devices.orEmpty().filter { !it.enrolled }.forEach { device ->
                    SettingsRow(device.name, summary = translate("Join to copy its sources and sign-ins"),
                        leading = { RufinIcon("rufin-phonelink-symbolic", null) }, trailing = next,
                        modifier = Modifier.clickable { welcome.join(device.id) })
                }
                SettingsRow(translate(if (setup.finding) "Searching..." else if (found.isEmpty()) "No Servers Found" else "Search Again"),
                    leading = { RufinIcon("rufin-view-refresh-symbolic", null) },
                    modifier = Modifier.clickable(enabled = !setup.finding, onClick = { setup.find(welcome != null) }))
            }
        } }
        item { SettingsGroup(translate("Add Manually")) {
            setup.providers.chunked(2).forEach { pair -> Row(horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                pair.forEach { provider ->
                    SettingsRow(provider.second, leading = { SourceIcon(provider.first, Modifier.size(24.dp)) },
                        modifier = Modifier.weight(1f).clickable { setup.selectProvider(provider.first) })
                }
            } }
        } }
        if (welcome != null) item { Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            TextButton(welcome.importBackup) { Text(translate("Import Backup")) }
        } }
    }
}

/** The first run's last page: the sync's progress over the lookup switches. */
@Composable
internal fun SourceLoadingPage(model: RufinConnection, welcome: SourceWelcome, close: () -> Unit) {
    Scaffold(bottomBar = {
        Surface {
            Button(close, Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 8.dp)) {
                Text(translate("Start Listening"))
            }
        }
    }) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding),
            contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
            item { Column(Modifier.padding(horizontal = 2.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                val failed = model.sourceOperation as? SourceOperation.Failed
                Text(translate(if (model.sourceBusy || failed != null) "While your library loads" else "Your library is ready"),
                    style = MaterialTheme.typography.headlineMedium)
                val progress = model.sourceProgress
                val total = progress?.total
                if (failed != null) io.github.screwys.rufin.ui.ErrorNotice(failed.message)
                else if (model.sourceBusy) {
                    if (progress != null && total != null && total > 0UL) LinearProgressIndicator(progress = {
                        (progress.completed.toFloat() / total.toFloat()).coerceIn(0f, 1f)
                    }, modifier = Modifier.fillMaxWidth())
                    else LinearProgressIndicator(Modifier.fillMaxWidth())
                    model.sourceStatus?.let {
                        Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            } }
            item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SettingsGroup(translate("Online Lookups")) {
                    welcome.preferences.applicationSettings?.let { state ->
                        SettingsSwitch(translate("External metadata lookup"), state.externalMetadataEnabled,
                            translate("Artist pictures, covers and tags from MusicBrainz, Last.fm and Wikidata")) {
                            welcome.preferences.changePreference { setExternalMetadataEnabled(it) }
                        }
                    }
                    welcome.player.settings?.lyrics?.let { lyrics ->
                        SettingsSwitch(translate("External lyrics lookup"), lyrics.externalLyricsEnabled,
                            translate("Lyrics from LRCLIB, NetEase, Genius and SimpMusic when a track has none")) {
                            welcome.player.lyricsPreference { setExternalLyricsEnabled(it) }
                        }
                    }
                    welcome.preferences.applicationSettings?.let { state ->
                        SettingsSwitch(translate("Private mode"), state.privateMode,
                            translate("Turns off lookups, scrobbling, play reporting and update checks")) {
                            welcome.preferences.changePreference { setPrivateMode(it) }
                        }
                    }
                }
                Text(translate("Rufin asks these services about the music you play. All of this can be changed later in Settings."),
                    Modifier.padding(horizontal = 12.dp), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
            } }
        }
    }
}
