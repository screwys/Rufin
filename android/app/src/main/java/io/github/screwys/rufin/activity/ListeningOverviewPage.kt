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

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
internal fun ListeningOverviewPage(model: RufinConnection, more: MoreConnection, player: PlayerConnection) {
    val report = more.report
    var periods by remember { mutableStateOf(false) }
    var customize by remember { mutableStateOf(false) }
    val colors = MaterialTheme.colorScheme
    val preferences = more.preferences
    val limit = preferences?.optInt("result_count", 3) ?: 3
    val dynamic = preferences?.optBoolean("dynamic_background", true) != false
    val background = rememberArtwork(report?.tracks?.firstOrNull()?.artworkIdentity, player,
        with(LocalDensity.current) { 256.dp.roundToPx() })
    val surface = if (dynamic && background != null) lerp(colors.surface, background.color, .12f) else colors.surface
    Surface(Modifier.fillMaxSize(), color = surface) {
        Box(Modifier.fillMaxSize()) {
            if (dynamic && preferences?.optBoolean("background_image", true) != false && background != null) {
                Image(background.bitmap.asImageBitmap(), null,
                    Modifier.fillMaxSize().blur(32.dp), alpha = .15f, contentScale = ContentScale.Crop)
            }
            Column(Modifier.fillMaxSize()) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    PlayerIconButton("rufin-go-previous-symbolic", "Previous", { more.movePeriod(true) })
                    TextButton({ periods = true }, Modifier.weight(1f)) {
                        Text(report?.takeIf { it.key == more.selectedPeriod.key }?.title ?: more.selectedPeriod.key)
                    }
                    PlayerIconButton("rufin-go-next-symbolic", "Next", { more.movePeriod(false) })
                    PlayerIconButton("rufin-sliders-2-symbolic", "Customize display", { customize = true })
                    IconButton({ more.exportOverview(colors.surface.luminance() < .5f, colors.onSurface.toArgb(),
                        colors.surface.toArgb(), colors.primary.toArgb()) },
                        enabled = report?.key == more.selectedPeriod.key && more.reportError == null && !more.reportLoading && !more.exporting) {
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
                        val headline = if (preferences?.optBoolean("show_rufin_in_headline", true) != false)
                            translate("Your {period} in Rufin") else translate("Your {period}")
                        Text(headline.replace("{period}", report.title), style = MaterialTheme.typography.headlineSmall)
                        val compare = preferences?.optBoolean("show_comparison", true) != false
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
                    listOf(Triple("Top tracks", "tracks", report.tracks), Triple("Top albums", "albums", report.albums), Triple("Top artists", "artists", report.artists)).forEach { (title, key, rows) ->
                        if (preferences?.optBoolean(key, true) != false && rows.isNotEmpty()) {
                            item { Text(translate(title), style = MaterialTheme.typography.titleLarge) }
                            items(rows.take(limit), key = { "$key:${it.mediaUri}" }) { row ->
                                val artwork = rememberArtwork(row.artworkIdentity, player, with(LocalDensity.current) { 48.dp.roundToPx() })
                                ListItem(headlineContent = { Text(row.title) }, supportingContent = { Text(row.subtitle) }, trailingContent = { Text(row.plays.toString()) },
                                    leadingContent = { Artwork(artwork, Modifier.size(48.dp).clip(RoundedCornerShape(8.dp))) },
                                    colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
                                    modifier = Modifier.clickable { model.openOverview(row, report.key) })
                            }
                        }
                    }
                    if (preferences?.optBoolean("genres", true) != false && report.genres.isNotEmpty()) {
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
    val preferences = more.preferences
    val dynamic = preferences?.optBoolean("dynamic_background", true) != false
    val limit = preferences?.optInt("result_count", 3) ?: 3
    SettingsGroup(translate("Listening overview")) {
        listOf("autoplay_first_track" to "Start playing automatically", "dynamic_background" to "Dynamic background",
            "background_image" to "Enable background image", "tracks" to "Top tracks", "artists" to "Top artists", "albums" to "Top albums",
            "genres" to "Top genres", "show_comparison" to "Show comparison", "show_rufin_in_headline" to "Show Rufin in headline").forEach { (key, label) ->
            val checked = preferences?.optBoolean(key, true) ?: true
            val enabled = more.bridge != null && preferences != null && (key != "background_image" || dynamic)
            SettingsRow(translate(label), modifier = Modifier.toggleable(checked, enabled = enabled, role = Role.Switch,
                onValueChange = { more.updatePreference(key, it) }),
                trailing = { Switch(checked, null, enabled = enabled) })
        }
        PreferenceChoice(translate("Results per section"), limit.toString(), listOf("3" to "3", "5" to "5", "10" to "10")) { more.updatePreference("result_count", it.toInt()) }
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
