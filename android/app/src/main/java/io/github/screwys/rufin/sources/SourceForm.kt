package io.github.screwys.rufin.sources

import io.github.screwys.rufin.shared.rufin_core.*

import androidx.compose.runtime.*
import io.github.screwys.rufin.shared.rufin_core.EditableSource
import io.github.screwys.rufin.core.translate
import io.github.screwys.rufin.shared.rufin_core.SourceOwner
import io.github.screwys.rufin.shared.sources.*

internal class SourceForm {
    var kind by mutableStateOf<String?>(null)
    var name by mutableStateOf("")
    var url by mutableStateOf("")
    var username by mutableStateOf("")
    var secret by mutableStateOf("")
    var verification by mutableStateOf("")
    var trustInvalidCertificate by mutableStateOf(false)
    var instantMix by mutableStateOf(true)
    var halfStars by mutableStateOf(false)
    var manualEmby by mutableStateOf(false)
    var subsonicAuth by mutableStateOf("password")
    var fileAuth by mutableStateOf("password")
    var integration by mutableStateOf(false)
    var domain by mutableStateOf("")
    var folders by mutableStateOf("")
    var exclusions by mutableStateOf("")
    var alternates by mutableStateOf("")
    var headers by mutableStateOf("")
    var certificate by mutableStateOf("")
    var encryptSmb by mutableStateOf(false)
    var replaceHeaders by mutableStateOf(true)

    fun load(editable: EditableSource) {
        val saved = editable
        kind = saved.source.kind; name = saved.source.name; integration = editable.integration
        replaceHeaders = false
        halfStars = saved.source.halfStarsEnabled
        url = saved.credentials.serverUrl
        username = saved.credentials.username
        trustInvalidCertificate = saved.credentials.trustInvalidCert
        subsonicAuth = saved.credentials.openSubsonicAuthentication?.name?.lowercase() ?: "password"
        instantMix = saved.useInstantMix ?: true
        manualEmby = !saved.embyConnect
        saved.plexSettings?.let { settings ->
            url = settings.addressOverride.orEmpty()
            trustInvalidCertificate = settings.trustInvalidCert
        }
        saved.fileSettings?.let(::loadFileSettings)
    }

    fun loadFileSettings(settings: FileSourceSettings) {
        url = settings.url; username = settings.username; domain = settings.domain
        fileAuth = settings.authentication.name.lowercase()
        trustInvalidCertificate = settings.trustInvalidCertificate
        encryptSmb = settings.requireSmbEncryption
        certificate = settings.certificatePem.orEmpty()
        folders = settings.folders.joinToString("\n")
        exclusions = settings.excludedFolders.joinToString("\n")
        alternates = settings.alternateUrls.joinToString("\n")
    }

    private fun lines(value: String) = value.lineSequence().map(String::trim).filter(String::isNotBlank).toList()
    private fun optional(value: String) = value.trim().takeIf(String::isNotEmpty)

    fun fileSettings() = FileSourceSettings(url.trim(), lines(alternates), lines(folders), lines(exclusions),
        username.trim(), domain.trim(), FileAuthentication.valueOf(fileAuth.uppercase()),
        trustInvalidCertificate, optional(certificate), encryptSmb)

    private fun fileHeaders() = headers.lineSequence().filter(String::isNotBlank).map { line ->
        val separator = line.indexOf(':')
        require(separator >= 0) { translate("One Name: Value header per line") }
        FileHeader(line.substring(0, separator).trim(), line.substring(separator + 1).trim())
    }.toList()

    fun fileCredentials() = FileCredentials(secret, fileHeaders())
    fun fileCredentialsEdit(editing: Boolean = true) = FileCredentialsEdit(
        secret.takeUnless { editing && it.isEmpty() }, if (!editing || replaceHeaders) fileHeaders() else null)

    suspend fun submit(setup: SourceOwner, sourceId: String?) {
        val credentials = CredentialInput(optional(name), url.trim(), username.trim(), secret, trustInvalidCertificate)
        when (kind) {
            "jellyfin", "emby" -> setup.saveJellyfinEmby(sourceId,
                if (kind == "emby") ServerKind.EMBY else ServerKind.JELLYFIN,
                credentials, instantMix, kind != "emby" || manualEmby)
            "navidrome", "subsonic" -> setup.saveSubsonic(sourceId,
                if (kind == "navidrome") SubsonicFlavor.NAVIDROME else SubsonicFlavor.SUBSONIC,
                SubsonicAuthentication.valueOf(subsonicAuth.uppercase()), credentials)
            "webdav", "smb" -> setup.saveFiles(sourceId, kind == "smb", integration, name.trim(),
                fileSettings(), fileCredentialsEdit(sourceId != null))
            "plex" -> setup.savePlex(sourceId ?: error(translate("Sign in to Plex first")),
                PlexSettingsInput(name.trim(), optional(url), trustInvalidCertificate))
            else -> error(translate("Select Source"))
        }
    }
}
