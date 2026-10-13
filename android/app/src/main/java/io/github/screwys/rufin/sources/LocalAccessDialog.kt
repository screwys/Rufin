package io.github.screwys.rufin.sources

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.ErrorNotice
import io.github.screwys.rufin.ui.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import io.github.screwys.rufin.shared.rufin_core.SourceLocalAccess

@Composable
internal fun LocalAccessDialog(model: RufinConnection, sourceId: String, samplePath: String? = null,
    onDismiss: () -> Unit, onSaved: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var bridge by remember { mutableStateOf<AndroidSourceSetup?>(null) }
    var root by remember { mutableStateOf("") }
    var serverPrefix by remember { mutableStateOf("") }
    var localPrefix by remember { mutableStateOf("") }
    var sample by remember { mutableStateOf(samplePath.orEmpty()) }
    var configured by remember { mutableStateOf(false) }
    var working by remember { mutableStateOf(true) }
    var failure by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(model, sourceId) {
        try {
            val runtime = model.connectedService().runtime.value?.getOrThrow()
                ?: error("Service is not connected")
            val setup = runtime.sourceSetup()
            bridge = setup
            val summary = setup.localAccess(sourceId)
            summary?.access?.let { access ->
                root = access.rootPath
                serverPrefix = access.serverPrefix.orEmpty()
                localPrefix = access.localPrefix.orEmpty()
                configured = true
            }
            if (samplePath == null) sample = summary?.status?.sampleSourcePath.orEmpty()
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { failure = error.userMessage() }
        finally { working = false }
    }
    DisposableEffect(Unit) { onDispose { bridge?.destroy() } }
    val chooseFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) try {
            context.contentResolver.takePersistableUriPermission(uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            root = uri.toString()
            failure = null
        } catch (error: Exception) { failure = error.userMessage() }
    }
    AlertDialog(onDismissRequest = { if (!working) onDismiss() },
        title = { Text(translate("Local File Mapping")) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (working) LinearProgressIndicator(Modifier.fillMaxWidth())
                failure?.let { ErrorNotice(it) }
                OutlinedButton({ chooseFolder.launch(root.takeIf { it.isNotBlank() }?.let(Uri::parse)) }, enabled = !working) {
                    Text(translate("Choose folder"))
                }
                if (root.isNotBlank()) Text(Uri.decode(Uri.parse(root).lastPathSegment.orEmpty()), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(serverPrefix, { serverPrefix = it }, Modifier.fillMaxWidth(),
                    label = { Text(translate("Server Prefix")) }, enabled = !working, singleLine = true)
                OutlinedTextField(localPrefix, { localPrefix = it }, Modifier.fillMaxWidth(),
                    label = { Text(translate("Local Prefix")) }, enabled = !working, singleLine = true)
                if (sample.isNotBlank()) OutlinedTextField(sample, {}, Modifier.fillMaxWidth(), readOnly = true,
                    label = { Text(translate("Server Sample")) })
                if (configured) TextButton({
                    bridge?.clearLocalAccess(sourceId)
                    onDismiss()
                }, enabled = !working) { Text(translate("Clear Mapping")) }
            }
        },
        confirmButton = { TextButton({
            scope.launch {
                working = true
                failure = null
                try {
                    val form = SourceLocalAccess(sourceId, root, serverPrefix.trim().ifEmpty { null },
                        localPrefix.trim().ifEmpty { null }, sample.takeIf { it.isNotBlank() })
                    bridge!!.saveLocalAccess(form)
                    onSaved()
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { failure = error.userMessage() }
                finally { working = false }
            }
        }, enabled = !working && bridge != null && root.isNotBlank()) { Text(translate("Save")) } },
        dismissButton = { TextButton(onDismiss, enabled = !working) { Text(translate("Cancel")) } })
}
