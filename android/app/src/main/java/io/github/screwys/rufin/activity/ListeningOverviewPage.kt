package io.github.screwys.rufin.activity

import io.github.screwys.rufin.settings.PreferenceChoice
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.ui.rememberArtwork
import io.github.screwys.rufin.ui.Artwork
import io.github.screwys.rufin.ui.ErrorRow
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.player.PlayerIconButton
import io.github.screwys.rufin.more.MoreConnection
import io.github.screwys.rufin.player.PlayerConnection
import io.github.screwys.rufin.app.RufinConnection

import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.screwys.rufin.ui.LocalPlayerBottomPadding
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.shared.rufin_core.ActivityOverviewSettings

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
internal fun ListeningOverviewPage(model: RufinConnection, more: MoreConnection, player: PlayerConnection) {
    val report = more.report
    var periods by remember { mutableStateOf(false) }
    var customize by remember { mutableStateOf(false) }
    val colors = MaterialTheme.colorScheme
    val preferences = more.preferences
    val limit = preferences?.resultCount?.toInt() ?: 3
    val dynamic = preferences?.dynamicBackground != false
    val background = rememberArtwork(report?.tracks?.firstOrNull()?.artworkBinding, player,
        with(LocalDensity.current) { 256.dp.roundToPx() })
    val surface = if (dynamic && background != null) lerp(colors.surface, background.color, .12f) else colors.surface
    Surface(Modifier.fillMaxSize(), color = surface) {
        Box(Modifier.fillMaxSize()) {
            if (dynamic && preferences?.backgroundImage != false && background != null) {
                Image(background.bitmap.asImageBitmap(), null,
                    Modifier.fillMaxSize().blur(32.dp), alpha = .15f, contentScale = ContentScale.Crop)
            }
            Column(Modifier.fillMaxSize()) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    PlayerIconButton("rufin-go-previous-symbolic", "Previous", { more.movePeriod(true) })
                    TextButton({ periods = true }, Modifier.weight(1f)) {
                        Text(more.reportTitle.takeIf { more.reportPeriod == more.selectedPeriod } ?: more.selectedPeriod.key)
                    }
                    PlayerIconButton("rufin-go-next-symbolic", "Next", { more.movePeriod(false) })
                    PlayerIconButton("rufin-sliders-2-symbolic", "Customize display", { customize = true })
                    IconButton({ more.exportOverview(colors.surface.luminance() < .5f, colors.onSurface.toArgb(),
                        colors.surface.toArgb(), colors.primary.toArgb()) },
                        enabled = more.reportPeriod == more.selectedPeriod && more.reportError == null && !more.reportLoading && !more.exporting) {
                        if (more.exporting) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        else RufinIcon("rufin-image-x-generic-symbolic", translate("Export PNG"))
                    }
                }
                Row(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilterChip(!more.yearly, { more.chooseYearly(false) }, label = { Text(translate("Month")) })
                    FilterChip(more.yearly, { more.chooseYearly(true) }, label = { Text(translate("Year")) })
                }
                if (more.reportLoading && report != null) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (more.reportError != null) ErrorRow(translate("Could not load listening overview"), translate("Retry"), more::retryOverview)
                if (report == null) Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    if (more.reportError == null) CircularProgressIndicator()
                } else LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(start = 12.dp, top = 12.dp, end = 12.dp, bottom = 12.dp + LocalPlayerBottomPadding.current), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    item {
                        val headline = if (preferences?.showRufinInHeadline != false)
                            translate("Your {period} in Rufin") else translate("Your {period}")
                        Text(headline.replace("{period}", more.reportTitle), style = MaterialTheme.typography.headlineSmall)
                        val compare = preferences?.showComparison != false
                        fun change(current: Long, previous: Long) = if (!compare || previous == 0L) "" else
                            "(%+.0f%%)".format((current.toDouble() - previous) * 100 / previous)
                        Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                OverviewStat("rufin-history-symbolic", "Hours", "%.1f".format(report.totals.durationMillis / 3600000.0),
                                    change(report.totals.durationMillis, report.previous.durationMillis), Modifier.weight(1f))
                                OverviewStat("rufin-media-playback-start-symbolic", "Plays", report.totals.plays.toString(),
                                    change(report.totals.plays, report.previous.plays), Modifier.weight(1f))
                            }
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                OverviewStat("rufin-artists-symbolic", "Artists", report.totals.artists.toString(),
                                    change(report.totals.artists, report.previous.artists), Modifier.weight(1f))
                                OverviewStat("rufin-tracks-symbolic", "Tracks", report.totals.tracks.toString(),
                                    change(report.totals.tracks, report.previous.tracks), Modifier.weight(1f))
                            }
                        }
                        if (report.totals.plays == 0L) Text(translate("No listening activity in this period"), Modifier.padding(vertical = 16.dp))
                    }
                    if (preferences?.tracks != false && report.tracks.isNotEmpty()) {
                        item { Text(translate("Top tracks"), style = MaterialTheme.typography.titleLarge) }
                        items(report.tracks.take(limit), key = { "track:${it.mediaUri}" }) { row ->
                            OverviewRow(row.title, "${row.artist} · ${row.album}", row.playCount, row.artworkBinding, player) {
                                model.openOverview(row.mediaUri, row.title, "track", more.reportPeriod!!.key)
                            }
                        }
                    }
                    if (preferences?.albums != false && report.albums.isNotEmpty()) {
                        item { Text(translate("Top albums"), style = MaterialTheme.typography.titleLarge) }
                        items(report.albums.take(limit), key = { "album:${it.mediaUri}" }) { row ->
                            OverviewRow(row.title, row.displayArtist, row.playCount, row.artworkBinding, player) {
                                model.openOverview(row.mediaUri, row.title, "album", more.reportPeriod!!.key)
                            }
                        }
                    }
                    if (preferences?.artists != false && report.artists.isNotEmpty()) {
                        item { Text(translate("Top artists"), style = MaterialTheme.typography.titleLarge) }
                        items(report.artists.take(limit), key = { "artist:${it.mediaUri}" }) { row ->
                            OverviewRow(row.name, "", row.playCount, row.artworkBinding, player) {
                                model.openOverview(row.mediaUri, row.name, "artist", more.reportPeriod!!.key)
                            }
                        }
                    }
                    if (preferences?.genres != false && report.genres.isNotEmpty()) {
                        item { Text(translate("Top genres"), style = MaterialTheme.typography.titleLarge) }
                        items(report.genres.take(limit), key = { it.name }) { genre -> Column {
                            Text("${genre.name} · ${genre.plays}")
                            LinearProgressIndicator(progress = { genre.plays.toFloat() / report.genres.first().plays.coerceAtLeast(1) }, modifier = Modifier.fillMaxWidth())
                        } }
                    }
                }
            }
        }
    }
    if (periods) ModalBottomSheet({ periods = false }) { LazyColumn {
        items(more.periods, key = { it.key }) { period -> ListItem(headlineContent = { Text(period.key) }, modifier = Modifier.clickable { more.choosePeriod(period); periods = false }) }
    } }
    if (customize) ModalBottomSheet({ customize = false }) {
        Column(Modifier.fillMaxWidth().heightIn(max = 560.dp).verticalScroll(rememberScrollState()).navigationBarsPadding().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp)) {
            Text(translate("Customize display"), style = MaterialTheme.typography.titleLarge)
            OverviewPreferences(more)
        }
    }
}

@Composable
internal fun OverviewPreferences(more: MoreConnection) {
    val preferences = more.preferences ?: return
    SettingsGroup(translate("Listening overview")) {
        OverviewSwitch(more, "Start playing automatically", preferences.autoplayFirstTrack) { copy(autoplayFirstTrack = it) }
        OverviewSwitch(more, "Dynamic background", preferences.dynamicBackground) { copy(dynamicBackground = it) }
        OverviewSwitch(more, "Enable background image", preferences.backgroundImage, preferences.dynamicBackground) { copy(backgroundImage = it) }
        OverviewSwitch(more, "Top tracks", preferences.tracks) { copy(tracks = it) }
        OverviewSwitch(more, "Top artists", preferences.artists) { copy(artists = it) }
        OverviewSwitch(more, "Top albums", preferences.albums) { copy(albums = it) }
        OverviewSwitch(more, "Top genres", preferences.genres) { copy(genres = it) }
        OverviewSwitch(more, "Show comparison", preferences.showComparison) { copy(showComparison = it) }
        OverviewSwitch(more, "Show Rufin in headline", preferences.showRufinInHeadline) { copy(showRufinInHeadline = it) }
        PreferenceChoice(translate("Results per section"), preferences.resultCount.toString(), listOf("3" to "3", "5" to "5", "10" to "10")) { value -> more.updatePreferences { it.copy(resultCount = value.toULong()) } }
    }
}

@Composable
private fun OverviewStat(icon: String, label: String, value: String, comparison: String, modifier: Modifier = Modifier) {
    Surface(modifier, shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.primaryContainer, contentColor = MaterialTheme.colorScheme.onPrimaryContainer) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            RufinIcon(icon, null, Modifier.size(24.dp))
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(value, style = MaterialTheme.typography.headlineMedium)
                Text(translate(label), style = MaterialTheme.typography.labelMedium)
                if (comparison.isNotEmpty()) Text(comparison, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
private fun OverviewSwitch(more: MoreConnection, label: String, checked: Boolean, enabled: Boolean = true,
    update: ActivityOverviewSettings.(Boolean) -> ActivityOverviewSettings) {
    SettingsRow(translate(label), modifier = Modifier.toggleable(checked, enabled = enabled && more.bridge != null,
        role = Role.Switch, onValueChange = { value -> more.updatePreferences { it.update(value) } }),
        trailing = { Switch(checked, null, enabled = enabled && more.bridge != null) })
}

@Composable
private fun OverviewRow(title: String, subtitle: String, plays: Long, binding: ByteArray?, player: PlayerConnection, open: () -> Unit) {
    val artwork = rememberArtwork(binding, player, with(LocalDensity.current) { 48.dp.roundToPx() })
    ListItem(headlineContent = { Text(title) }, supportingContent = { Text(subtitle) }, trailingContent = { Text(plays.toString()) },
        leadingContent = { Artwork(artwork, Modifier.size(48.dp).clip(RoundedCornerShape(8.dp))) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier.clickable(onClick = open))
}
