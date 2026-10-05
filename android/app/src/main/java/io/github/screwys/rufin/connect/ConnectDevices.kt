package io.github.screwys.rufin.connect

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsRow

@Composable
internal fun ConnectDeviceRow(device: AndroidConnectDevice, connect: ConnectConnection, controls: () -> Unit, remove: () -> Unit,
    joining: Boolean = connect.state?.profile == null) {
    var menu by remember { mutableStateOf(false) }
    val pair = { if (joining) connect.join(device.id) else connect.action(AndroidConnectAction.Invite(device.id)) }
    SettingsRow(device.name, summary = if (device.reachable) device.connection else translate("Offline"),
        leading = { RufinIcon("rufin-phonelink-symbolic", null) }, trailing = {
        Row(verticalAlignment = Alignment.CenterVertically) {
        if (!device.enrolled) TextButton(pair, enabled = !connect.pending) { Text(translate(if (joining) "Join profile" else "Invite")) }
        else {
            if (device.hasPlayback) TextButton({ connect.action(AndroidConnectAction.Continue(device.id)) },
                enabled = device.reachable && !connect.pending) { Text(translate("Continue")) }
        Box {
            IconButton({ menu = true }) { RufinIcon("rufin-view-more-symbolic", translate("More actions")) }
            DropdownMenu(menu, { menu = false }) {
                DropdownMenuItem(text = { Text(translate("Test connection")) }, enabled = !connect.pending,
                    leadingIcon = { RufinIcon("rufin-view-refresh-symbolic", null) },
                    onClick = { connect.action(AndroidConnectAction.TestConnection(device.id)); menu = false })
                DropdownMenuItem(text = { Text(translate("Remove")) }, enabled = !connect.pending,
                    leadingIcon = { RufinIcon("rufin-user-trash-symbolic", null) },
                    onClick = { remove(); menu = false })
            }
        }
        }
        }
    }, modifier = Modifier.clickable(enabled = !connect.pending && (!device.enrolled || device.reachable)) {
        if (device.enrolled) controls() else pair()
    })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConnectDeviceControls(device: AndroidConnectDevice, connect: ConnectConnection, dismiss: () -> Unit) {
    var volume by remember { mutableFloatStateOf(1f) }
    val current = connect.state?.devices?.firstOrNull { it.id == device.id }
    val enabled = current?.reachable == true && !connect.pending
    ModalBottomSheet(onDismissRequest = dismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.65f).imePadding().padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(current?.name ?: device.name, style = MaterialTheme.typography.titleLarge)
                    Text(if (current?.reachable == true) current.connection else translate("Offline"),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                IconButton(dismiss) { RufinIcon("rufin-go-down-symbolic", translate("Close")) }
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (current?.hasPlayback == true) FilledTonalButton({ connect.action(AndroidConnectAction.Continue(device.id)) },
                Modifier.fillMaxWidth(), enabled = enabled) { Text(translate("Continue")) }
            val controls = listOf(
                Triple("rufin-media-skip-backward-symbolic", "Previous Track", AndroidConnectControl.Previous),
                Triple("rufin-media-playback-start-symbolic", "Play", AndroidConnectControl.Play),
                Triple("rufin-media-playback-pause-symbolic", "Pause", AndroidConnectControl.Pause),
                Triple("rufin-process-stop-symbolic", "Stop", AndroidConnectControl.Stop),
                Triple("rufin-media-skip-forward-symbolic", "Next Track", AndroidConnectControl.Next),
            )
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
                controls.forEach { (icon, title, command) -> IconButton({ connect.action(AndroidConnectAction.Control(device.id, command)) }, enabled = enabled) {
                    RufinIcon(icon, translate(title), Modifier.size(28.dp))
                } }
            }
            Text(translate("Volume"), style = MaterialTheme.typography.titleSmall)
            Slider(volume, { volume = it }, onValueChangeFinished = {
                connect.action(AndroidConnectAction.Control(device.id, AndroidConnectControl.Volume(volume.toDouble())))
            }, enabled = enabled)
            Spacer(Modifier.height(16.dp))
            }
        }
    }
}
