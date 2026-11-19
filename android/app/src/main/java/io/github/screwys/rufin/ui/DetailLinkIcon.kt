package io.github.screwys.rufin.ui

import io.github.screwys.rufin.R

import androidx.compose.foundation.Image
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource

@Composable
internal fun DetailLinkIcon(iconId: String, modifier: Modifier) {
    val resource = when (iconId) {
        "app.rufin.rufin.source.jellyfin" -> R.drawable.source_jellyfin
        "app.rufin.rufin.source.emby" -> R.drawable.source_emby
        "app.rufin.rufin.source.navidrome" -> R.drawable.source_navidrome
        "app.rufin.rufin.source.opensubsonic" -> R.drawable.source_opensubsonic
        "app.rufin.rufin.source.plex" -> R.drawable.source_plex
        "app.rufin.rufin.source.webdav" -> R.drawable.source_webdav
        "app.rufin.rufin.source.smb" -> R.drawable.source_smb
        "app.rufin.rufin.external.lastfm" -> R.drawable.external_lastfm
        "app.rufin.rufin.external.musicbrainz" -> R.drawable.external_musicbrainz
        else -> null
    }
    if (resource != null) Image(painterResource(resource), null, modifier)
    else RufinIcon(if (iconId == "app.rufin.rufin-symbolic") "rufin-cover-fallback-symbolic" else iconId, null, modifier)
}
