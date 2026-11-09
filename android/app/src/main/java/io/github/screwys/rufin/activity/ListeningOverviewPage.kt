package io.github.screwys.rufin.activity

import io.github.screwys.rufin.ui.SegmentedPill
import io.github.screwys.rufin.ui.ToolButton
import io.github.screwys.rufin.ui.PageLoading
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow

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
import io.github.screwys.rufin.ui.CoverShape
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
        with(LocalDensity.current) { 256.dp.roundToPx() }).image
    val surface = if (dynamic && background != null) lerp(colors.surface, background.color, .12f) else colors.surface
    Surface(Modifier.fillMaxSize(), color = surface) {
        Box(Modifier.fillMaxSize()) {
            if (dynamic && preferences?.backgroundImage != false && background != null) {
                Image(background.bitmap.asImageBitmap(), null,
                    Modifier.fillMaxSize().blur(32.dp), alpha = .15f, contentScale = ContentScale.Crop)
            }
            Column(Modifier.fillMaxSize()) {
                // The period in a pill between its steps, the tools beside it, and Month or Year under them.
                Row(Modifier.fillMaxWidth().padding(start = 12.dp, end = 10.dp, top = 8.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Surface(Modifier.weight(1f).height(44.dp), shape = CircleShape, color = colors.surfaceContainerHigh) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            PlayerIconButton("rufin-go-previous-symbolic", "Previous", { more.movePeriod(true) }, iconSize = 20.dp)
                            Text(more.reportTitle.takeIf { more.reportPeriod == more.selectedPeriod } ?: more.selectedPeriod.key,
                                Modifier.weight(1f).clip(CircleShape).clickable { periods = true }.padding(vertical = 10.dp),
                                textAlign = TextAlign.Center, style = MaterialTheme.typography.titleSmall, maxLines = 1,
                                overflow = TextOverflow.Ellipsis)
                            PlayerIconButton("rufin-go-next-symbolic", "Next", { more.movePeriod(false) }, iconSize = 20.dp)
                        }
                    }
                    ToolButton("rufin-sliders-2-symbolic", "Customize display") { customize = true }
                    Box(contentAlignment = Alignment.Center) {
                        ToolButton("rufin-image-x-generic-symbolic", "Export PNG",
                            more.reportPeriod == more.selectedPeriod && more.reportError == null && !more.reportLoading && !more.exporting) {
                            more.exportOverview(colors.surface.luminance() < .5f, colors.onSurface.toArgb(),
                                colors.surface.toArgb(), colors.primary.toArgb())
                        }
                        if (more.exporting) CircularProgressIndicator(Modifier.size(40.dp), strokeWidth = 2.dp)
                    }
                }
                SegmentedPill(listOf(false, true), more.yearly, more::chooseYearly,
                    Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp), equal = true) { yearly, _ ->
                    Text(translate(if (yearly) "Year" else "Month"), style = MaterialTheme.typography.titleSmall)
                }
                if (more.reportLoading && report != null) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (more.reportError != null) ErrorRow(translate("Could not load listening overview"), translate("Retry"), more::retryOverview)
                if (report == null) { if (more.reportError == null) PageLoading() }
                else LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(start = 16.dp, top = 8.dp, end = 16.dp, bottom = 16.dp + LocalPlayerBottomPadding.current),
                    verticalArrangement = Arrangement.spacedBy(24.dp)) {
                    item {
                        val headline = if (preferences?.showRufinInHeadline != false)
                            translate("Your {period} in Rufin") else translate("Your {period}")
                        Text(headline.replace("{period}", more.reportTitle), style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.ExtraBold)
                        val compare = preferences?.showComparison != false
                        fun change(current: Long, previous: Long) = if (!compare || previous == 0L) "" else
                            "%+.0f%%".format((current.toDouble() - previous) * 100 / previous)
                        Column(Modifier.padding(top = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
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
                        if (report.totals.plays == 0L) Text(translate("No listening activity in this period"), Modifier.padding(top = 16.dp),
                            color = colors.onSurfaceVariant)
                    }
                    if (preferences?.tracks != false && report.tracks.isNotEmpty()) item("tracks") {
                        SettingsGroup(translate("Top tracks")) {
                            report.tracks.take(limit).forEachIndexed { rank, row ->
                                OverviewRow(rank + 1, row.title, "${row.artist} · ${row.album}", row.state.playCount, row.artworkBinding, player) {
                                    model.openOverview(row.mediaUri, row.title, "track", more.reportPeriod!!.key)
                                }
                            }
                        }
                    }
                    if (preferences?.albums != false && report.albums.isNotEmpty()) item("albums") {
                        SettingsGroup(translate("Top albums")) {
                            report.albums.take(limit).forEachIndexed { rank, row ->
                                OverviewRow(rank + 1, row.title, row.artist, row.state.playCount, row.artworkBinding, player) {
                                    model.openOverview(row.mediaUri, row.title, "album", more.reportPeriod!!.key)
                                }
                            }
                        }
                    }
                    if (preferences?.artists != false && report.artists.isNotEmpty()) item("artists") {
                        SettingsGroup(translate("Top artists")) {
                            report.artists.take(limit).forEachIndexed { rank, row ->
                                OverviewRow(rank + 1, row.name, "", row.state.playCount, row.artworkBinding, player) {
                                    model.openOverview(row.mediaUri, row.name, "artist", more.reportPeriod!!.key)
                                }
                            }
                        }
                    }
                    if (preferences?.genres != false && report.genres.isNotEmpty()) item("genres") {
                        SettingsGroup(translate("Top genres")) {
                            val most = report.genres.first().plays.coerceAtLeast(1)
                            report.genres.take(limit).forEach { genre ->
                                Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(5.dp), color = colors.surfaceContainer) {
                                    Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                                        Row(verticalAlignment = Alignment.CenterVertically) {
                                            Text(genre.name, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge, maxLines = 1,
                                                overflow = TextOverflow.Ellipsis)
                                            PlayCount(genre.plays)
                                        }
                                        LinearProgressIndicator(progress = { genre.plays.toFloat() / most },
                                            Modifier.fillMaxWidth().height(6.dp), drawStopIndicator = {}, gapSize = 0.dp,
                                            strokeCap = StrokeCap.Round)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if (periods) ModalBottomSheet({ periods = false }) {
        LazyColumn(Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)).padding(horizontal = 16.dp),
            contentPadding = PaddingValues(bottom = 24.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            items(more.periods, key = { it.key }) { period ->
                val picked = period == more.selectedPeriod
                SettingsRow(period.key, selected = picked, modifier = Modifier.clickable { more.choosePeriod(period); periods = false })
            }
        }
    }
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

/** A total as a tile: its icon in an accent circle, the number large, and how it changed. */
@Composable
private fun OverviewStat(icon: String, label: String, value: String, comparison: String, modifier: Modifier = Modifier) {
    val colors = MaterialTheme.colorScheme
    Surface(modifier, shape = RoundedCornerShape(20.dp), color = colors.surfaceContainerHigh) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(36.dp).clip(CircleShape).background(colors.primary), contentAlignment = Alignment.Center) {
                    RufinIcon(icon, null, Modifier.size(18.dp), colors.onPrimary, glyph = true)
                }
                Spacer(Modifier.weight(1f))
                if (comparison.isNotEmpty()) Text(comparison, style = MaterialTheme.typography.labelLarge, color = colors.primary)
            }
            Column {
                Text(value, style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
                Text(translate(label), style = MaterialTheme.typography.labelLarge, color = colors.onSurfaceVariant)
            }
        }
    }
}

/** How often something played, as a small pill. */
@Composable
private fun PlayCount(plays: Long) {
    Surface(shape = CircleShape, color = MaterialTheme.colorScheme.secondaryContainer) {
        Row(Modifier.padding(horizontal = 10.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            RufinIcon("rufin-media-playback-start-symbolic", null, Modifier.size(12.dp), glyph = true)
            Text(plays.toString(), style = MaterialTheme.typography.labelMedium)
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

/** A ranked entry in its section's box: the place, the cover, the names and the plays. */
@Composable
private fun OverviewRow(rank: Int, title: String, subtitle: String, plays: Long, binding: ByteArray?, player: PlayerConnection, open: () -> Unit) {
    val artwork = rememberArtwork(binding, player, with(LocalDensity.current) { 48.dp.roundToPx() })
    SettingsRow(title, summary = subtitle.ifBlank { null }, leading = {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(rank.toString(), Modifier.width(20.dp), textAlign = TextAlign.Center, style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold, color = MaterialTheme.colorScheme.primary)
            Artwork(artwork, Modifier.size(48.dp).clip(CoverShape))
        }
    }, trailing = { PlayCount(plays) }, modifier = Modifier.clickable(onClick = open))
}
