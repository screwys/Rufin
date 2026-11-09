package io.github.screwys.rufin.player

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.screwys.rufin.browse.MediaMenuBackdrop
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.settings.LocalReduceMotion
import io.github.screwys.rufin.shared.playback.SleepTimerChoice
import io.github.screwys.rufin.shared.playback.SleepTimerView
import io.github.screwys.rufin.shared.rufin_core.SleepTimerRequest
import io.github.screwys.rufin.shared.rufin_core.SleepTimerText
import io.github.screwys.rufin.shared.rufin_core.sleepTimerPresets
import io.github.screwys.rufin.shared.rufin_core.sleepTimerText
import io.github.screwys.rufin.ui.RufinIcon
import kotlinx.coroutines.delay
import kotlin.math.roundToInt

/** The running timer and its texts, counting a running time down every second. */
@Composable
private fun rememberSleepTimer(player: PlayerConnection): Pair<SleepTimerView?, SleepTimerText?> {
    val timer by remember(player) { derivedStateOf { player.playback?.controls?.sleepTimer } }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(timer) {
        now = System.currentTimeMillis()
        while (timer?.endsAtUnixMillis != null) { delay(1_000 - now % 1_000); now = System.currentTimeMillis() }
    }
    return timer to timer?.let { sleepTimerText(it, now) }
}

/** The row atop the player's More sheet: Play random, Sleep timer and Auto DJ. */
@Composable
internal fun PlayerMenuActions(player: PlayerConnection, onRandom: () -> Unit, onSleepTimer: () -> Unit, onAutoDjSettings: () -> Unit) {
    val autoDj by remember(player) { derivedStateOf { player.playback?.controls?.autoDjEnabled == true } }
    val (timer, text) = rememberSleepTimer(player)
    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
        Row(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
            PlayerMenuTile("rufin-random-symbolic", translate("Play random"), Modifier.weight(1f), onClick = onRandom)
            PlayerMenuTile("rufin-sleep-timer-symbolic", text?.short?.ifEmpty { null } ?: translate("Sleep timer"),
                Modifier.weight(1f), active = timer != null, onClick = onSleepTimer)
            PlayerMenuTile("rufin-auto-dj-symbolic", translate("Auto DJ"), Modifier.weight(1f), active = autoDj,
                onClick = player::autoDj, onLongClick = onAutoDjSettings, longClickLabel = translate("Settings"))
        }
        HorizontalDivider(Modifier.padding(top = 4.dp, bottom = 8.dp))
    }
}

/** The sleep timer's own sheet, on the playing track's cover as the More sheet. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SleepTimerSheet(player: PlayerConnection, onDismiss: () -> Unit) {
    val (timer, text) = rememberSleepTimer(player)
    ModalBottomSheet(onDismiss, containerColor = Color.Transparent, contentColor = MaterialTheme.colorScheme.onSurface,
        contentWindowInsets = { WindowInsets(0, 0, 0, 0) }, dragHandle = null,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        MediaMenuBackdrop(player.currentItem?.artworkBinding, player) {
            Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(top = 8.dp, bottom = 16.dp)) {
                BottomSheetDefaults.DragHandle(Modifier.align(Alignment.CenterHorizontally))
                Text(translate("Sleep timer"), Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.titleLarge)
                SleepTimerPanel(player, timer, text, onDismiss)
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PlayerMenuTile(icon: String, label: String, modifier: Modifier, active: Boolean = false,
    onClick: () -> Unit, onLongClick: (() -> Unit)? = null, longClickLabel: String? = null) {
    val tint by animateColorAsState(if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
        tween(if (LocalReduceMotion.current) 0 else 180), label = "playerMenuTile")
    Surface(modifier.clip(RoundedCornerShape(16.dp)).combinedClickable(onClick = onClick, onLongClick = onLongClick,
        onLongClickLabel = longClickLabel), color = Color.Transparent, contentColor = tint) {
        Column(Modifier.padding(vertical = 10.dp, horizontal = 4.dp), horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            RufinIcon(icon, null, Modifier.size(26.dp), tint = tint)
            Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelLarge,
                fontWeight = if (active) FontWeight.SemiBold else null)
        }
    }
}

/** The running timer, then its kinds: a preset or the slider picks the value, Start starts it. */
@Composable
private fun SleepTimerPanel(player: PlayerConnection, timer: SleepTimerView?, text: SleepTimerText?, onDone: () -> Unit) {
    val presets = remember { sleepTimerPresets() }
    var tracks by remember(timer?.choice is SleepTimerChoice.Tracks) { mutableStateOf(timer?.choice is SleepTimerChoice.Tracks) }
    var finishTrack by remember(timer?.finishTrack) { mutableStateOf(timer?.finishTrack == true) }
    // The running timer's value, or 30 minutes or the end of this track; a preset fills it, Start starts it.
    var custom by remember(tracks) { mutableFloatStateOf(when (val choice = timer?.choice) {
        is SleepTimerChoice.Minutes -> if (tracks) 1f else choice.minutes.toFloat()
        is SleepTimerChoice.Tracks -> if (tracks) choice.tracks.toFloat() else 30f
        null -> if (tracks) 1f else 30f
    }) }
    fun start(choice: SleepTimerChoice) {
        player.sleepTimer(SleepTimerRequest.Start(choice, choice is SleepTimerChoice.Minutes && finishTrack))
        onDone()
    }
    // The More sheet's shade, so the buttons sit in the cover's colours.
    val shade = if (MaterialTheme.colorScheme.surface.luminance() < .5f) Color.Black else Color.White
    Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        if (timer != null && text != null) {
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(text.countdown, fontSize = 44.sp, fontWeight = FontWeight.Light, textAlign = TextAlign.Center)
                if (text.caption.isNotEmpty()) Text(text.caption, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilledTonalButton({ player.sleepTimer(SleepTimerRequest.Extend) }, Modifier.weight(1f),
                    colors = ButtonDefaults.filledTonalButtonColors(containerColor = shade.copy(alpha = .14f),
                        contentColor = MaterialTheme.colorScheme.onSurface)) { Text(text.extend) }
                TextButton({ player.sleepTimer(SleepTimerRequest.Off); onDone() }, Modifier.weight(1f),
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text(translate("Turn off")) }
            }
        }
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            listOf(false to "Time", true to "Tracks").forEachIndexed { index, (kind, label) ->
                SegmentedButton(tracks == kind, { tracks = kind }, SegmentedButtonDefaults.itemShape(index, 2), icon = {},
                    colors = SegmentedButtonDefaults.colors(
                        activeContainerColor = MaterialTheme.colorScheme.primary,
                        activeContentColor = MaterialTheme.colorScheme.onPrimary,
                        activeBorderColor = MaterialTheme.colorScheme.primary,
                        inactiveContainerColor = Color.Transparent,
                        inactiveContentColor = MaterialTheme.colorScheme.onSurface,
                        inactiveBorderColor = MaterialTheme.colorScheme.onSurface.copy(alpha = .3f))) {
                    Text(translate(label))
                }
            }
        }
        val choices = if (tracks) presets.tracks.map { SleepTimerChoice.Tracks(it) to if (it == 1u) translate("End of this track") else it.toString() }
            else presets.minutes.map { SleepTimerChoice.Minutes(it) to translate("{minutes} min").replace("{minutes}", it.toString()) }
        // Four columns; End of this track takes two, as GTK's grid.
        val rows = choices.fold(mutableListOf<MutableList<Pair<SleepTimerChoice, String>>>()) { rows, choice ->
            val units = rows.lastOrNull()?.sumOf { if (it.first == SleepTimerChoice.Tracks(1u)) 2 else 1 } ?: 4
            if (units >= 4) rows.add(mutableListOf(choice)) else rows.last().add(choice)
            rows
        }
        rows.forEach { row -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            row.forEach { (choice, label) ->
                val wide = choice == SleepTimerChoice.Tracks(1u)
                val value = when (choice) { is SleepTimerChoice.Minutes -> choice.minutes; is SleepTimerChoice.Tracks -> choice.tracks }
                val current = custom.roundToInt().toUInt() == value
                val colors = if (current) ButtonDefaults.buttonColors()
                    else ButtonDefaults.filledTonalButtonColors(containerColor = shade.copy(alpha = .14f),
                        contentColor = MaterialTheme.colorScheme.onSurface)
                Button({ custom = value.toFloat() }, Modifier.weight(if (wide) 2f else 1f).height(40.dp), shape = RoundedCornerShape(10.dp),
                    colors = colors, contentPadding = PaddingValues(horizontal = 4.dp)) {
                    Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        } }
        val most = if (tracks) 20f else 120f
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            IconButton({ custom = (custom - 1).coerceAtLeast(1f) }) { RufinIcon("rufin-value-decrease-symbolic", translate("Less")) }
            Slider(custom, { custom = it.roundToInt().toFloat() }, Modifier.weight(1f), valueRange = 1f..most)
            IconButton({ custom = (custom + 1).coerceAtMost(most) }) { RufinIcon("rufin-list-add-symbolic", translate("More")) }
            Text(if (tracks) custom.roundToInt().toString() else translate("{minutes} min").replace("{minutes}", custom.roundToInt().toString()),
                Modifier.widthIn(min = 52.dp), textAlign = TextAlign.End, fontWeight = FontWeight.SemiBold)
        }
        if (!tracks) Row(verticalAlignment = Alignment.CenterVertically) {
            Text(translate("Finish the last track"), Modifier.weight(1f))
            Switch(finishTrack, {
                finishTrack = it
                if (timer?.choice is SleepTimerChoice.Minutes) player.sleepTimer(SleepTimerRequest.FinishTrack(it))
            })
        }
        Button({ start(if (tracks) SleepTimerChoice.Tracks(custom.roundToInt().toUInt()) else SleepTimerChoice.Minutes(custom.roundToInt().toUInt())) },
            Modifier.fillMaxWidth()) { Text(translate("Start")) }
    }
}
