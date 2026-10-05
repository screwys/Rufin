package io.github.screwys.rufin.sources

import androidx.compose.runtime.*
import io.github.screwys.rufin.core.AndroidSourceEditable
import io.github.screwys.rufin.core.translate
import org.json.JSONArray
import org.json.JSONObject

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

    fun load(editable: AndroidSourceEditable) {
        kind = editable.kind; name = editable.name; integration = editable.integration
        replaceHeaders = false
        if (editable.form.isBlank()) return
        val root = JSONObject(editable.form)
        halfStars = root.optJSONObject("source")?.optBoolean("half_stars_enabled") == true
        val credentials = root.optJSONObject("credentials") ?: JSONObject()
        url = credentials.optString("server_url")
        username = credentials.optString("username")
        trustInvalidCertificate = credentials.optBoolean("trust_invalid_cert")
        subsonicAuth = credentials.optString("open_subsonic_authentication", "password").takeUnless { it == "null" } ?: "password"
        instantMix = root.optBoolean("use_instant_mix", true)
        manualEmby = !root.optBoolean("emby_connect")
        root.optJSONObject("plex_settings")?.let { settings ->
            url = settings.optString("address_override").takeUnless { it == "null" }.orEmpty()
            trustInvalidCertificate = settings.optBoolean("trust_invalid_cert")
        }
        root.optJSONObject("file_settings")?.let(::loadFileSettings)
    }

    fun loadFileSettings(settings: JSONObject) {
        url = settings.optString("url")
        username = settings.optString("username")
        domain = settings.optString("domain")
        fileAuth = settings.optString("authentication", "password")
        trustInvalidCertificate = settings.optBoolean("trust_invalid_certificate")
        encryptSmb = settings.optBoolean("require_smb_encryption")
        certificate = settings.optString("certificate_pem").takeUnless { it == "null" }.orEmpty()
        folders = settings.optJSONArray("folders").lines()
        exclusions = settings.optJSONArray("excluded_folders").lines()
        alternates = settings.optJSONArray("alternate_urls").lines()
    }

    private fun JSONArray?.lines() = if (this == null) "" else (0 until length()).joinToString("\n") { getString(it) }
    private fun lines(value: String) = JSONArray(value.lineSequence().map(String::trim).filter(String::isNotBlank).toList())
    private fun optional(value: String): Any = value.trim().takeIf(String::isNotEmpty) ?: JSONObject.NULL

    fun fileSettings() = JSONObject().put("url", url.trim()).put("username", username.trim())
        .put("domain", domain.trim()).put("authentication", fileAuth)
        .put("alternate_urls", lines(alternates)).put("folders", lines(folders)).put("excluded_folders", lines(exclusions))
        .put("trust_invalid_certificate", trustInvalidCertificate).put("certificate_pem", optional(certificate))
        .put("require_smb_encryption", encryptSmb)

    fun fileCredentials(editing: Boolean = false): JSONObject {
        val pairs = JSONArray()
        if (!editing || replaceHeaders) headers.lineSequence().filter(String::isNotBlank).forEach { line ->
            val separator = line.indexOf(':')
            require(separator >= 0) { translate("One Name: Value header per line") }
            pairs.put(JSONArray().put(line.substring(0, separator).trim()).put(line.substring(separator + 1).trim()))
        }
        return JSONObject().put("secret", if (editing && secret.isEmpty()) JSONObject.NULL else secret)
            .put("headers", if (editing && !replaceHeaders) JSONObject.NULL else pairs)
    }

    fun submission(sourceId: String?): String {
        val editing = sourceId != null
        val credentials = JSONObject().put("source_name", optional(name)).put("server_url", url.trim())
            .put("username", username.trim()).put("secret", secret).put("trust_invalid_cert", trustInvalidCertificate)
        val body = JSONObject()
        if (editing) body.put("source_id", sourceId)
        val type = when (kind) {
            "jellyfin", "emby" -> {
                body.put("credentials", credentials).put("use_instant_mix", instantMix)
                if (editing) body.put("connect_manually", kind != "emby" || manualEmby) else body.put("kind", kind)
                "jellyfin_emby"
            }
            "navidrome", "subsonic" -> {
                body.put("credentials", credentials).put("kind", if (kind == "navidrome") "navidrome" else "open_subsonic")
                    .put("authentication", subsonicAuth)
                "open_subsonic"
            }
            "webdav", "smb" -> {
                body.put("name", name.trim()).put("settings", fileSettings()).put("credentials", fileCredentials(editing))
                if (editing) "files" else if (kind == "webdav") "web_dav" else "smb"
            }
            "plex" -> {
                body.put("settings", JSONObject().put("name", name.trim()).put("address_override", optional(url))
                    .put("trust_invalid_cert", trustInvalidCertificate))
                "plex"
            }
            else -> error(translate("Select Source"))
        }
        return JSONObject().put("type", type).put("data", body).toString()
    }
}
