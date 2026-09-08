use localization::msgid;
pub fn source_kind_title(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "jellyfin" => msgid("Jellyfin"),
        "emby" => msgid("Emby"),
        "plex" => msgid("Plex"),
        "navidrome" => msgid("Navidrome"),
        "subsonic" => msgid("OpenSubsonic"),
        "local" => msgid("Local"),
        "webdav" => msgid("WebDAV"),
        "smb" => msgid("SMB / Samba"),
        _ => return None,
    })
}

use adw::prelude::*;
use app_identity::{DISPLAY_NAME, STABLE_APP_ID};
use localization::{tr, tr_with};
use rufin_core::runtime::source::SourceSummary;
pub fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        source_kind_title(&source.kind).map_or_else(|| source.kind.clone(), tr)
    } else {
        name.to_string()
    }
}
pub fn configured_source_icon_name(source: &SourceSummary) -> &'static str {
    source_kind_icon_name(&source.kind).unwrap_or("rufin-network-server-symbolic")
}
pub fn configure_ownership_toggle(
    button: &gtk::ToggleButton,
    source: Option<&SourceSummary>,
    belongs_to_source: bool,
) {
    let source_icon = source.map(configured_source_icon_name);
    let source_name = source.map(configured_source_display_name);
    button.set_sensitive(source.is_some());
    button.set_active(belongs_to_source && source.is_some());
    let update = move |button: &gtk::ToggleButton| {
        let current = button.is_active() && source_icon.is_some();
        button.set_icon_name(if current {
            source_icon.unwrap_or(STABLE_APP_ID)
        } else {
            STABLE_APP_ID
        });
        let tooltip = if current {
            tr_with(
                "This is owned by {source}. Regular playlists are synced to your remote.",
                &[("source", source_name.as_deref().unwrap_or(DISPLAY_NAME))],
            )
        } else {
            tr(
                "This is owned by Rufin. It can include entries from all configured sources but it is not synced to remotes.",
            )
        };
        button.set_tooltip_text(Some(&tooltip));
    };
    update(button);
    button.connect_toggled(update);
}

pub fn source_kind_icon_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "jellyfin" => "io.github.screwys.Rufin.source.jellyfin",
        "emby" => "io.github.screwys.Rufin.source.emby",
        "plex" => "io.github.screwys.Rufin.source.plex",
        "navidrome" => "io.github.screwys.Rufin.source.navidrome",
        "subsonic" => "io.github.screwys.Rufin.source.opensubsonic",
        "local" => "io.github.screwys.Rufin-symbolic",
        "webdav" => "io.github.screwys.Rufin.source.webdav",
        "smb" => "io.github.screwys.Rufin.source.smb",
        _ => return None,
    })
}
