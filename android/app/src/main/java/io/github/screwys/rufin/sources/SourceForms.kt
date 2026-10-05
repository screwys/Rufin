package io.github.screwys.rufin.sources

import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import io.github.screwys.rufin.core.*
import io.github.screwys.rufin.ui.RufinIcon
import io.github.screwys.rufin.settings.SettingsRow
import io.github.screwys.rufin.settings.SettingsGroup
import io.github.screwys.rufin.settings.settingsButtonColors
import io.github.screwys.rufin.settings.settingsChipColors

@Composable
internal fun SourceTextField(label: String, value: String, change: (String) -> Unit, enabled: Boolean = true,
    secret: Boolean = false, multiline: Boolean = false, hint: String? = null) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
    Text(translate(label), Modifier.padding(start = 8.dp), style = MaterialTheme.typography.bodyMedium)
    OutlinedTextField(value, change, Modifier.fillMaxWidth(), enabled = enabled,
        shape = RoundedCornerShape(16.dp), singleLine = !multiline, minLines = if (multiline) 3 else 1,
        colors = OutlinedTextFieldDefaults.colors(
            focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            disabledContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            focusedBorderColor = Color.Transparent,
            unfocusedBorderColor = Color.Transparent,
            disabledBorderColor = Color.Transparent),
        supportingText = hint?.let { { Text(translate(it)) } },
        visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false,
            keyboardType = when { secret -> KeyboardType.Password; label.contains("address", ignoreCase = true) -> KeyboardType.Uri; else -> KeyboardType.Text }))
    }
}

@Composable
internal fun SourceSwitch(label: String, checked: Boolean, change: (Boolean) -> Unit, enabled: Boolean = true) {
    SettingsRow(translate(label), modifier = Modifier.toggleable(checked, enabled = enabled, role = Role.Switch, onValueChange = change),
        trailing = { Switch(checked, null, enabled = enabled) })
}

@Composable
internal fun SourceRatingSettings(setup: SourceSetupConnection) {
    SettingsGroup(translate("Library")) {
        SourceSwitch("Enable partial star ratings", setup.form.halfStars, setup::setHalfStars, !setup.working)
    }
}

@Composable
internal fun SourceCredentials(form: SourceForm, working: Boolean, editing: Boolean, apiKey: Boolean = false) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        SourceTextField("Server Address", form.url, { form.url = it }, !working)
        if (!apiKey) SourceTextField("Username", form.username, { form.username = it }, !working)
        SourceTextField(if (apiKey) "API key" else "Password", form.secret, { form.secret = it }, !working,
            secret = true, hint = if (editing && !apiKey) "Leave the password empty to keep the saved credential" else null)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun SourceOptions(title: String, choices: List<Pair<String, String>>, selected: String, enabled: Boolean = true, choose: (String) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
    Text(translate(title), Modifier.padding(start = 12.dp, top = 12.dp, bottom = 6.dp),
        style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        choices.forEach { (id, title) -> FilterChip(selected == id, { choose(id) }, label = { Text(translate(title)) }, enabled = enabled, colors = settingsChipColors()) }
    }
    }
}

@Composable
internal fun SourceChoices(choices: List<AndroidSourceChoice>, selected: UInt, choose: (UInt) -> Unit) {
    SettingsGroup {
    choices.forEach { choice ->
        SettingsRow(choice.title, summary = choice.address,
            leading = { RadioButton(choice.index == selected, { choose(choice.index) }) },
            modifier = Modifier.clickable { choose(choice.index) })
    }
    }
}

@Composable
internal fun SourceDiscovery(setup: SourceSetupConnection) {
    LaunchedEffect(setup, setup.ready, setup.form.kind) {
        if (setup.ready) setup.discover()
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
    val expected = when (setup.form.kind) { "jellyfin" -> AndroidSourceDiscoveryProvider.JELLYFIN; "emby" -> AndroidSourceDiscoveryProvider.EMBY; else -> AndroidSourceDiscoveryProvider.PLEX }
    setup.discovery?.takeIf { it.provider == expected }?.let { discovery ->
        if (discovery.phase == "searching") LinearProgressIndicator(Modifier.fillMaxWidth())
        if (discovery.phase == "empty") Text(translate("No Servers Found"), color = MaterialTheme.colorScheme.onSurfaceVariant)
        discovery.failure?.let { io.github.screwys.rufin.ui.ErrorNotice(it) }
        discovery.servers.forEach { server ->
            ListItem(headlineContent = { Text(server.title) }, supportingContent = { Text(server.address.orEmpty()) },
                modifier = Modifier.clickable { setup.form.url = server.address.orEmpty() })
        }
    }
    }
}

@Composable
internal fun PlexSourceForm(setup: SourceSetupConnection, editing: Boolean, browser: (String) -> Unit) {
    val form = setup.form
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (editing) {
            SourceTextField("Server Address Override (optional)", form.url, { form.url = it }, !setup.working)
        } else if (setup.choosingProfiles) {
            Text(translate("Home Profile"), style = MaterialTheme.typography.titleMedium)
            SourceChoices(setup.profiles, setup.selectedProfile) { setup.selectedProfile = it; setup.homePin = "" }
            if (setup.profiles.firstOrNull { it.index == setup.selectedProfile }?.pinRequired == true)
                SourceTextField("Profile PIN", setup.homePin, { setup.homePin = it }, !setup.working, secret = true)
            Button(setup::choosePlexProfile, enabled = !setup.working && setup.profiles.isNotEmpty()) { Text(translate("Choose Server")) }
        } else if (setup.authorized) {
            Text(translate("Server"), style = MaterialTheme.typography.titleMedium)
            if (setup.choices.isEmpty()) Text(translate("No Servers Found"))
            SourceChoices(setup.choices, setup.selectedServer) { setup.selectedServer = it }
            TextButton(setup::changePlexProfile, enabled = !setup.working) { Text(translate("Change Profile")) }
            SourceTextField("Server Address Override (optional)", form.url, { form.url = it }, !setup.working)
        } else {
            SourceTextField("Username", form.username, { form.username = it }, !setup.working)
            SourceTextField("Password", form.secret, { form.secret = it }, !setup.working, secret = true)
            SourceTextField("Verification Code (optional)", form.verification, { form.verification = it }, !setup.working)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton({ setup.plexLogin(true, browser) }, enabled = !setup.working, colors = settingsButtonColors()) { Text(translate("Sign In in Browser")) }
                OutlinedButton({ setup.plexLogin(false, browser) }, enabled = !setup.working && form.username.isNotBlank(), colors = settingsButtonColors()) { Text(translate("Sign in")) }
            }
            setup.savedAccounts.forEach { account ->
            ListItem(headlineContent = { Text(account.title) },
                    leadingContent = { RufinIcon("rufin-document-open-symbolic", null) },
                    modifier = Modifier.clickable(enabled = !setup.working) { setup.useSaved(account.index) })
            }
            SourceDiscovery(setup)
        }
        SourceSwitch("Verify server certificate", !form.trustInvalidCertificate, { form.trustInvalidCertificate = !it }, !setup.working)
    }
}

@Composable
internal fun FileSourceForm(setup: SourceSetupConnection, editing: Boolean, browser: (String) -> Unit) {
    val form = setup.form
    val smb = form.kind == "smb"
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SourceTextField("Server Address", form.url, { form.url = it }, !setup.working)
        SourceOptions("Authentication", if (smb) listOf("password" to "Username and password", "anonymous" to "Guest / public access")
            else listOf("password" to "Username and password", "anonymous" to "Anonymous", "bearer" to "Bearer token"), form.fileAuth, enabled = !setup.working) { form.fileAuth = it }
        if (form.fileAuth == "password") SourceTextField("Username", form.username, { form.username = it }, !setup.working)
        if (form.fileAuth != "anonymous") SourceTextField(if (form.fileAuth == "bearer") "Bearer token" else "Password",
            form.secret, { form.secret = it }, !setup.working, secret = true,
            hint = if (editing && form.fileAuth == "password") "Leave the password empty to keep the saved credential" else null)
        if (smb) {
            SourceTextField("SMB Domain", form.domain, { form.domain = it }, !setup.working)
            OutlinedButton(setup::smbShares, enabled = !setup.working && form.url.isNotBlank(), colors = settingsButtonColors()) { Text(translate("Find shares")) }
            setup.choices.forEach { share -> ListItem(headlineContent = { Text(share.title) }, supportingContent = { Text(share.address.orEmpty()) },
                modifier = Modifier.clickable { form.url = share.address.orEmpty() }) }
            SourceSwitch("Require SMB encryption", form.encryptSmb, { form.encryptSmb = it }, !setup.working)
        } else {
            OutlinedButton({ setup.nextcloud(browser) }, enabled = !setup.working && form.url.isNotBlank(), colors = settingsButtonColors()) { Text(translate("Sign in with Nextcloud")) }
            SourceSwitch("Verify server certificate", !form.trustInvalidCertificate, { form.trustInvalidCertificate = !it }, !setup.working)
        }
        SettingsGroup(translate("Folders")) {
            SourceTextField("Selected folders", form.folders, { form.folders = it }, !setup.working, multiline = true)
            SourceTextField("Excluded folders", form.exclusions, { form.exclusions = it }, !setup.working, multiline = true)
        }
        SettingsGroup {
            SourceTextField("Alternate addresses", form.alternates, { form.alternates = it }, !setup.working, multiline = true)
            if (!smb) {
                if (editing) SourceSwitch("Replace saved custom headers", form.replaceHeaders, { form.replaceHeaders = it }, !setup.working)
                SourceTextField("Custom HTTP headers", form.headers, { form.headers = it }, !setup.working && (!editing || form.replaceHeaders),
                    multiline = true, hint = "One Name: Value header per line")
                SourceTextField("Certificate authority (PEM)", form.certificate, { form.certificate = it }, !setup.working, multiline = true)
            }
        }
    }
}
