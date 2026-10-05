package io.github.screwys.rufin

import io.github.screwys.rufin.platform.AndroidPermissions
import io.github.screwys.rufin.settings.AppTheme
import io.github.screwys.rufin.app.LibraryScreen
import io.github.screwys.rufin.settings.PreferencesConnection
import io.github.screwys.rufin.app.RufinConnection

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.viewModels
import androidx.activity.compose.setContent
import androidx.activity.result.IntentSenderRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.media3.common.util.UnstableApi
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.flow.filterNotNull
import io.github.screwys.rufin.diagnostics.FrameDiagnostics

@androidx.annotation.OptIn(UnstableApi::class)
@OptIn(ExperimentalMaterial3Api::class)
class MainActivity : ComponentActivity() {
    private val connection: RufinConnection by viewModels()
    private val documentConsent = registerForActivityResult(ActivityResultContracts.StartIntentSenderForResult()) {
        AndroidPermissions.complete(it.resultCode == RESULT_OK)
    }
    private val documentPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
        AndroidPermissions.complete(it)
    }
    private val openDocuments = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == RESULT_OK) {
            val intent = result.data ?: return@registerForActivityResult
            val uris = intent.clipData?.let { clip -> (0 until clip.itemCount).map { clip.getItemAt(it).uri } }
                ?: listOfNotNull(intent.data)
            connection.openDocuments(uris, intent.flags)
        }
    }
    private val openFolder = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == RESULT_OK) {
            val intent = result.data ?: return@registerForActivityResult
            val uri = intent.data ?: return@registerForActivityResult
            connection.addFolder(uri, intent.flags)
        }
    }
    private val saveDiagnosticLog = registerForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        if (uri != null) connection.saveDiagnosticLog(uri)
    }
    private val saveOverview = registerForActivityResult(ActivityResultContracts.CreateDocument("image/png")) { uri ->
        connection.saveOverview(uri)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val model = connection
        setContent {
            val measureFrames = BuildConfig.DEBUG || model.debugLogging
            DisposableEffect(measureFrames) {
                val diagnostics = if (measureFrames)
                    FrameDiagnostics(window, (1_000_000_000.0 / windowManager.defaultDisplay.refreshRate).toLong()) else null
                onDispose { diagnostics?.close() }
            }
            LaunchedEffect(connection) {
                lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                    snapshotFlow { connection.overviewExportName }.filterNotNull().collect { name ->
                        saveOverview.launch(name)
                        connection.overviewExportLaunched()
                    }
                }
            }
            val preferences = remember { PreferencesConnection(connection, this@MainActivity) }
            LaunchedEffect(preferences) {
                try { preferences.observe() }
                catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
                catch (error: Exception) { connection.runAction { throw error } }
            }
            AppTheme(preferences) {
                CompositionLocalProvider(io.github.screwys.rufin.ui.LocalFeedback provides { message -> connection.showFeedback(message) }) {
                LibraryScreen(connection, preferences,
                    onOpen = {
                        openDocuments.launch(Intent(Intent.ACTION_OPEN_DOCUMENT)
                            .setType("*/*")
                            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION))
                    },
                    onAddFolder = {
                        openFolder.launch(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                                Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION))
                    },
                    onSaveLog = { saveDiagnosticLog.launch("rufin-debug.log") },
                )
                }
            }
        }
    }

    override fun onDestroy() {
        if (!isChangingConfigurations) AndroidPermissions.cancel()
        super.onDestroy()
    }

    override fun onStart() {
        super.onStart()
        AndroidPermissions.attach { prompt ->
            when (prompt) {
                is AndroidPermissions.Prompt.Consent -> documentConsent.launch(IntentSenderRequest.Builder(prompt.sender).build())
                is AndroidPermissions.Prompt.Permission -> documentPermission.launch(prompt.permission)
            }
        }
    }

    override fun onStop() {
        AndroidPermissions.detach()
        super.onStop()
    }

}
