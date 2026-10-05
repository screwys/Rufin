package io.github.screwys.rufin.player

import androidx.compose.animation.Crossfade
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.AndroidOutputKind
import io.github.screwys.rufin.core.AndroidOutput
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.LocalReduceMotion
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
internal fun PlayerOutputSheet(player: PlayerConnection, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val service = player.service
    val routes = service?.outputs?.routes?.collectAsState()?.value.orEmpty()
    val scope = rememberCoroutineScope()
    val choices = player.outputChoices
    val reduceMotion = LocalReduceMotion.current
    val outputs = choices?.outputs.orEmpty()
    val connect = player.model.connect.state
    var loading by remember { mutableStateOf(true) }
    var pending by remember { mutableStateOf<String?>(null) }
    val playback = player.playback
    var volumePreview by remember(playback?.outputKind, playback?.outputId) { mutableStateOf<Float?>(null) }
    val volume = volumePreview ?: playback?.volume?.toFloat() ?: 1f
    LaunchedEffect(playback?.volume, volumePreview) {
        val preview = volumePreview ?: return@LaunchedEffect
        if (playback != null && abs(playback.volume - preview) < 0.0001) volumePreview = null
    }
    LaunchedEffect(player.bridge) {
        loading = true
        try { player.outputs() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
        finally { loading = false }
    }
    LaunchedEffect(choices?.discoveryError) {
        choices?.discoveryError?.let { player.reportError(Exception(it)) }
    }
    fun select(key: String, action: suspend () -> Unit) {
        pending = key
        scope.launch {
            try { action() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { if (pending == key) player.reportError(error) }
            finally { if (pending == key) pending = null }
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        LazyColumn(Modifier.fillMaxWidth().fillMaxHeight(0.65f), contentPadding = PaddingValues(bottom = 24.dp)) {
            item {
                Text(translate("Audio output"), Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center, style = MaterialTheme.typography.titleLarge)
                Text(translate("Volume"), Modifier.padding(horizontal = 24.dp, vertical = 8.dp), style = MaterialTheme.typography.titleMedium)
                val primary = MaterialTheme.colorScheme.primary
                Box(Modifier.padding(horizontal = 12.dp).fillMaxWidth().height(56.dp), contentAlignment = Alignment.Center) {
                    Slider(volume.coerceIn(0f, 1f), { value -> volumePreview = value; player.volume(value.toDouble()) },
                        Modifier.fillMaxSize(), enabled = playback != null,
                        thumb = { Box(Modifier.width(2.dp).height(24.dp).background(MaterialTheme.colorScheme.onSurface, RoundedCornerShape(1.dp))) },
                        track = { slider -> Canvas(Modifier.fillMaxWidth().height(56.dp)) {
                            val radius = CornerRadius(12.dp.toPx())
                            drawRoundRect(primary.copy(alpha = .15f), cornerRadius = radius)
                            drawRoundRect(primary.copy(alpha = .4f), size = Size(size.width * slider.value.coerceIn(0f, 1f), size.height), cornerRadius = radius)
                        } })
                    Text("${(volume * 100).roundToInt()}%", style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onPrimaryContainer)
                }
                if (loading) Row(Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 8.dp),
                    horizontalArrangement = Arrangement.End) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                }
            }
            items(routes, key = { "android:${it.id}" }) { route ->
                val key = "android:${route.id}"
                OutputRouteRow(route.name, "rufin-speaker-front-symbolic", route.selected && playback?.localOutput != false,
                    route.enabled && pending != key, pending == key, modifier = if (reduceMotion) Modifier else Modifier.animateItem()) { select(key) { player.selectAndroidOutput(route.id) } }
            }
            items(outputs.filter { it.kind != AndroidOutputKind.CONNECT && (it.kind != AndroidOutputKind.LOCAL || routes.isEmpty()) },
                key = { "rufin:${it.kind}:${it.id}" }) { output ->
                val key = "rufin:${output.kind}:${output.id}"
                val selected = if (playback == null) output.selected else output.kind == playback.outputKind && output.id == playback.outputId
                OutputRouteRow(output.name, if (output.kind == AndroidOutputKind.LOCAL) "rufin-speaker-front-symbolic" else "rufin-waves-and-screen-symbolic",
                    selected, output.available && pending != key, pending == key, modifier = if (reduceMotion) Modifier else Modifier.animateItem()) { select(key) { player.selectOutput(output) } }
            }
            val devices = connect?.let { state ->
                state.devices.map { device ->
                    AndroidOutput(device.id, device.name, AndroidOutputKind.CONNECT,
                        playback?.let { it.outputKind == AndroidOutputKind.CONNECT && it.outputId == device.id }
                            ?: outputs.any { it.kind == AndroidOutputKind.CONNECT && it.id == device.id && it.selected },
                        state.enabled && device.enrolled && device.reachable, device.hasPlayback)
                }
            } ?: outputs.filter { it.kind == AndroidOutputKind.CONNECT }
            if (devices.isNotEmpty()) item {
                Text(translate("Rufin Connect"), Modifier.padding(horizontal = 24.dp, vertical = 8.dp), style = MaterialTheme.typography.titleMedium)
            }
            items(devices, key = { "connect:${it.id}" }) { output ->
                val key = "connect:${output.id}"
                OutputRouteRow(output.name, "rufin-phonelink-symbolic", output.selected, output.available && pending != key, pending == key,
                    modifier = if (reduceMotion) Modifier else Modifier.animateItem()) { select(key) { player.selectOutput(output) } }
            }
            item {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                FilledTonalButton({ service?.outputs?.openBluetoothSettings(context) },
                    enabled = service != null) {
                    RufinIcon("rufin-waves-and-screen-symbolic", null, Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(translate("Connect a device"))
                }
                }
            }
        }
    }
}

@Composable
private fun OutputRouteRow(name: String, icon: String, selected: Boolean, enabled: Boolean, pending: Boolean,
    modifier: Modifier = Modifier, select: () -> Unit) {
    val motionMillis = if (LocalReduceMotion.current) 0 else 120
    val color by animateColorAsState(when {
        selected -> MaterialTheme.colorScheme.primary
        enabled -> MaterialTheme.colorScheme.onSurface
        else -> MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f)
    }, animationSpec = tween(motionMillis), label = "outputSelection")
    val primary = MaterialTheme.colorScheme.primary
    Surface(modifier.padding(horizontal = 12.dp, vertical = 4.dp), shape = RoundedCornerShape(12.dp),
        color = primary.copy(alpha = if (selected) .3f else .1f)) {
    ListItem(headlineContent = { Text(name, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        leadingContent = { RufinIcon(icon, null, Modifier.size(28.dp), color) },
        trailingContent = {
            Crossfade(if (pending) 2 else if (selected) 1 else 0, Modifier.size(24.dp), tween(motionMillis), label = "outputStatus") { state ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    if (state == 2) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    else if (state == 1) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(24.dp), color)
                }
            }
        }, modifier = Modifier.clickable(enabled = enabled, onClick = select))
    }
}
