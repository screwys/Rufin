package io.github.screwys.rufin.ui

import io.github.screwys.rufin.R

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp

@Composable
internal fun SourceIcon(kind: String, modifier: Modifier = Modifier.size(24.dp)) {
    val resource = when (kind) {
        "jellyfin" -> R.drawable.source_jellyfin
        "emby" -> R.drawable.source_emby
        "navidrome" -> R.drawable.source_navidrome
        "subsonic", "opensubsonic" -> R.drawable.source_opensubsonic
        "plex" -> R.drawable.source_plex
        "webdav" -> R.drawable.source_webdav
        "smb" -> R.drawable.source_smb
        else -> null
    }
    if (resource != null) Image(painterResource(resource), null, modifier)
    else RufinIcon(if (kind == "local") "rufin-cover-fallback-symbolic" else "rufin-network-server-symbolic", null, modifier)
}
