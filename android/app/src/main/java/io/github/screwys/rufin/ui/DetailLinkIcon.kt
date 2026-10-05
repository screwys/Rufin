package io.github.screwys.rufin.ui

import io.github.screwys.rufin.R

import androidx.compose.foundation.Image
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource

@Composable
internal fun DetailLinkIcon(iconId: String, modifier: Modifier) {
    val resource = when (iconId) {
        "io.github.screwys.Rufin.source.jellyfin" -> R.drawable.source_jellyfin
        "io.github.screwys.Rufin.source.emby" -> R.drawable.source_emby
        "io.github.screwys.Rufin.source.navidrome" -> R.drawable.source_navidrome
        "io.github.screwys.Rufin.source.opensubsonic" -> R.drawable.source_opensubsonic
        "io.github.screwys.Rufin.source.plex" -> R.drawable.source_plex
        "io.github.screwys.Rufin.source.webdav" -> R.drawable.source_webdav
        "io.github.screwys.Rufin.source.smb" -> R.drawable.source_smb
        "io.github.screwys.Rufin.external.lastfm" -> R.drawable.external_lastfm
        "io.github.screwys.Rufin.external.musicbrainz" -> R.drawable.external_musicbrainz
        else -> null
    }
    if (resource != null) Image(painterResource(resource), null, modifier)
    else RufinIcon(if (iconId == "io.github.screwys.Rufin-symbolic") "rufin-cover-fallback-symbolic" else iconId, null, modifier)
}
