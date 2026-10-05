package io.github.screwys.rufin.connect

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*

@Composable
internal fun ConnectPairingPrompt(model: RufinConnection, onOpen: () -> Unit) {
    val connect = model.connect
    val pairing by remember(connect) { derivedStateOf { connect.state?.pairing } }
    pairing?.let { request ->
        AlertDialog(onDismissRequest = { connect.action(AndroidConnectAction.CancelPairing) },
            title = { Text(request.name) }, text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(request.emoji.joinToString(" "), style = MaterialTheme.typography.headlineSmall)
                    Text(translate("Compare these emoji on both devices"))
                    if (!request.joining) Text(translate("Approving gives this device access to your profile and saved source logins."),
                        style = MaterialTheme.typography.bodySmall)
                }
            }, confirmButton = { TextButton({
                if (!request.approved) connect.action(AndroidConnectAction.Pair(request.session, true, false))
                if (!connect.pageVisible) onOpen()
            }, enabled = !connect.pending && !request.approved) { Text(if (request.approved) translate("Waiting for device") else translate("Approve")) } },
            dismissButton = { TextButton({ connect.action(AndroidConnectAction.CancelPairing) }, enabled = !connect.pending) { Text(translate("Cancel")) } })
    }
}
