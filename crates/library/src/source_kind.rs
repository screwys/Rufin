use crate::DeliveryKind;

/// The kinds of source Rufin connects to, and what each one does. Saved settings, the Store
/// and Connect records keep the kind as its name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceKind {
    Local,
    Webdav,
    Smb,
    Jellyfin,
    Emby,
    Plex,
    /// An OpenSubsonic server with Navidrome's extensions.
    Navidrome,
    Subsonic,
    Peer,
}

impl SourceKind {
    const NAMES: [(Self, &'static str); 9] = [
        (Self::Local, "local"),
        (Self::Webdav, "webdav"),
        (Self::Smb, "smb"),
        (Self::Jellyfin, "jellyfin"),
        (Self::Emby, "emby"),
        (Self::Plex, "plex"),
        (Self::Navidrome, "navidrome"),
        (Self::Subsonic, "subsonic"),
        (Self::Peer, "peer"),
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(kind, _)| *kind)
    }

    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(kind, _)| *kind == self)
            .map_or("", |(_, name)| name)
    }

    /// Rufin reads the files itself, so albums come from tags and plays from Rufin's listens.
    pub fn file_library(self) -> bool {
        matches!(self, Self::Local | Self::Webdav | Self::Smb)
    }

    pub fn subsonic(self) -> bool {
        matches!(self, Self::Navidrome | Self::Subsonic)
    }

    /// Whether a playlist can belong to the source: its server stores it.
    pub fn owns_playlists(self) -> bool {
        matches!(self, Self::Jellyfin | Self::Emby | Self::Plex) || self.subsonic()
    }

    pub fn playlist_public(self) -> bool {
        matches!(self, Self::Jellyfin | Self::Emby) || self.subsonic()
    }

    pub fn playlist_tracks_can_repeat(self) -> bool {
        !matches!(self, Self::Jellyfin | Self::Plex)
    }

    /// Whether the server takes favorites, and whether it takes ratings. Plex has no
    /// favorites and Emby no numeric ratings; Rufin holds those values itself.
    pub fn delivers(self, delivery: DeliveryKind) -> bool {
        match delivery {
            DeliveryKind::Favorite => {
                matches!(self, Self::Jellyfin | Self::Emby) || self.subsonic()
            }
            DeliveryKind::Rating => matches!(self, Self::Jellyfin | Self::Plex) || self.subsonic(),
        }
    }

    /// Rufin keeps its copy of an accepted rating (0-100) that the server holds differently:
    /// Plex ratings, and half stars on a server that stores whole stars.
    pub fn keeps_rating(self, value: i64) -> bool {
        self == Self::Plex || (self.subsonic() && value % 20 != 0)
    }

    /// Half stars are on without the per-source setting.
    pub fn half_stars(self) -> bool {
        matches!(self, Self::Jellyfin | Self::Emby | Self::Plex)
    }
}
