package io.github.screwys.rufin.settings

import android.app.Activity
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.app.RufinConnection
import io.github.screwys.rufin.core.translate
import org.json.JSONObject

@Composable
internal fun BackupSettingsPage(model: RufinConnection, preferences: PreferencesConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val settings = preferences.applicationSettings?.optJSONObject("backup")
    if (settings == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    var enable by remember { mutableStateOf(false) }
    var password by remember { mutableStateOf("") }
    val context = LocalContext.current
    val schedule = settings.optJSONObject("schedule") ?: JSONObject()
    val destination = settings.optString("destination_uri").takeUnless { it == "null" || it.isEmpty() }
    val chooseFolder = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == Activity.RESULT_OK) result.data?.data?.let { uri -> model.runAction {
            val granted = result.data!!.flags
            val read = granted and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0
            val write = granted and Intent.FLAG_GRANT_WRITE_URI_PERMISSION != 0
            when {
                read && write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                read -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                write -> context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            }
            preferences.nativePreferences().setApplicationPreference("backup.destination_uri", JSONObject.quote(uri.toString()))
            preferences.refreshApplicationSettings()
        } }
    }
    SettingsPage {
        SettingsGroup(translate("Automatic Backups")) {
            SettingsSwitch(translate("Enable automatic backups"), settings.optBoolean("enabled")) {
                if (it) enable = true else preferences.changePreference("backup.enabled", false)
            }
            SettingsRow(translate("Backup Folder"), summary = destination ?: translate("Rufin data folder"), trailing = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (destination != null) IconButton({ preferences.changePreference("backup.destination_uri", JSONObject.NULL) }) {
                        io.github.screwys.rufin.ui.RufinIcon("rufin-view-refresh-symbolic", translate("Use Rufin data folder"), Modifier.size(20.dp))
                    }
                    OutlinedButton({ chooseFolder.launch(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)) },
                        colors = settingsButtonColors()) { Text(translate("Choose")) }
                }
            })
            SettingsNumber(translate("Number of backups"), settings.optDouble("retention_count", 2.0), 1.0..4294967295.0) {
                preferences.changePreference("backup.retention_count", (it as Number).toLong())
            }
            val frequency = schedule.optString("frequency", "Daily").takeUnless { it == "Off" } ?: "Daily"
            PreferenceChoice(translate("Schedule"), frequency, listOf("Daily" to translate("Daily"), "Weekly" to translate("Weekly")),
                select = { preferences.changePreference("backup.schedule.frequency", it) })
            if (frequency == "Weekly") PreferenceChoice(translate("Weekday"), schedule.optInt("weekday").toString(),
                listOf("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday").mapIndexed { index, day -> index.toString() to translate(day) },
                select = { preferences.changePreference("backup.schedule.weekday", it.toInt()) })
            PreferenceChoice(translate("Hour"), schedule.optInt("hour", 2).toString(),
                (0..23).map { it.toString() to it.toString() }, select = { preferences.changePreference("backup.schedule.hour", it.toInt()) })
            SettingsSwitch(translate("Encrypt backups"), settings.optBoolean("encrypt")) { preferences.changePreference("backup.encrypt", it) }
            if (settings.optBoolean("encrypt")) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(password, { password = it }, Modifier.fillMaxWidth(), label = { Text(translate("Password")) },
                        singleLine = true, visualTransformation = PasswordVisualTransformation(), colors = settingsTextFieldColors())
                    OutlinedButton({ model.runAction {
                        model.connectedService().runtime.value!!.getOrThrow().backups().use { it.saveSchedulePassword(password) }
                        password = ""
                        model.showFeedback(translate("Scheduled backup password saved"))
                    } }, enabled = password.isNotEmpty(), colors = settingsButtonColors()) { Text(translate("Save")) }
                }
            }
        }
        BackupContentsControls(settings.optJSONObject("contents") ?: defaultBackupContents()) { preferences.changePreference("backup.contents", it) }
    }
    if (enable) BackupPasswordDialog(false, dismiss = { enable = false }) { passphrase ->
        enable = false
        model.runAction {
            if (passphrase != null) model.connectedService().runtime.value!!.getOrThrow().backups().use { it.saveSchedulePassword(passphrase) }
            val next = JSONObject(settings.toString()).put("enabled", true).put("encrypt", passphrase != null)
            preferences.nativePreferences().setApplicationPreference("backup", next.toString())
            preferences.refreshApplicationSettings()
        }
    }
}
