package io.github.screwys.rufin.settings

import io.github.screwys.rufin.shared.rufin_core.ApplicationPreferenceEdit
import io.github.screwys.rufin.shared.backup.BackupFrequency

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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
internal fun BackupSettingsPage(model: RufinConnection, preferences: PreferencesConnection) {
    LaunchedEffect(preferences) { preferences.refreshApplicationSettings() }
    val settings = preferences.applicationSettings?.backup
    if (settings == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    var enable by remember { mutableStateOf(false) }
    var password by remember { mutableStateOf("") }
    val context = LocalContext.current
    val schedule = settings.schedule
    val destination = settings.destinationUri
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
            withContext(Dispatchers.IO) { preferences.nativeSettings().editApplicationPreferences(ApplicationPreferenceEdit.Backup(settings.copy(destinationUri = uri.toString()))) }
            preferences.refreshApplicationSettings()
        } }
    }
    SettingsPage {
        SettingsGroup(translate("Automatic Backups")) {
            SettingsSwitch(translate("Enable automatic backups"), settings.enabled) {
                if (it) enable = true else preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(enabled = false)))
            }
            SettingsRow(translate("Backup Folder"), summary = destination ?: translate("Rufin data folder"), trailing = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (destination != null) IconButton({ preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(destinationUri = null))) }) {
                        io.github.screwys.rufin.ui.RufinIcon("rufin-view-refresh-symbolic", translate("Use Rufin data folder"), Modifier.size(20.dp))
                    }
                    OutlinedButton({ chooseFolder.launch(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)) },
                        colors = settingsButtonColors()) { Text(translate("Choose")) }
                }
            })
            SettingsNumber(translate("Number of backups"), settings.retentionCount.toDouble(), 1.0..4294967295.0) {
                preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(retentionCount = (it as Number).toLong().toUInt())))
            }
            val frequency = schedule.frequency.takeUnless { it == BackupFrequency.OFF } ?: BackupFrequency.DAILY
            PreferenceChoice(translate("Schedule"), frequency.name, listOf("DAILY" to translate("Daily"), "WEEKLY" to translate("Weekly")),
                select = { preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(schedule = schedule.copy(frequency = BackupFrequency.valueOf(it))))) })
            if (frequency == BackupFrequency.WEEKLY) PreferenceChoice(translate("Weekday"), schedule.weekday.toString(),
                listOf("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday").mapIndexed { index, day -> index.toString() to translate(day) },
                select = { preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(schedule = schedule.copy(weekday = it.toUByte())))) })
            PreferenceChoice(translate("Hour"), schedule.hour.toString(),
                (0..23).map { it.toString() to it.toString() }, select = { preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(schedule = schedule.copy(hour = it.toUByte())))) })
            SettingsSwitch(translate("Encrypt backups"), settings.encrypt) { preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(encrypt = it))) }
            if (settings.encrypt) {
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
        BackupContentsControls(settings.contents) { preferences.changePreference(ApplicationPreferenceEdit.Backup(settings.copy(contents = it))) }
    }
    if (enable) BackupPasswordDialog(false, dismiss = { enable = false }) { passphrase ->
        enable = false
        model.runAction {
            if (passphrase != null) model.connectedService().runtime.value!!.getOrThrow().backups().use { it.saveSchedulePassword(passphrase) }
            val next = settings.copy(enabled = true, encrypt = passphrase != null)
            withContext(Dispatchers.IO) { preferences.nativeSettings().editApplicationPreferences(ApplicationPreferenceEdit.Backup(next)) }
            preferences.refreshApplicationSettings()
        }
    }
}
