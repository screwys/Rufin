use ::library::AlbumView;

use localization::msgid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlbumReleaseKind {
    Album,
    Ep,
    Single,
    Collection,
    Other,
}

pub fn album_release_kind(album: &AlbumView) -> AlbumReleaseKind {
    if album.metadata.is_compilation == Some(true) {
        return AlbumReleaseKind::Collection;
    }

    let types = album
        .metadata
        .release_types
        .iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if types.is_empty() || types.iter().any(|value| value == "album") {
        return AlbumReleaseKind::Album;
    }
    if types.iter().any(|value| {
        matches!(
            value.as_str(),
            "compilation" | "compilations" | "collection" | "collections"
        )
    }) {
        return AlbumReleaseKind::Collection;
    }
    if types
        .iter()
        .any(|value| matches!(value.as_str(), "ep" | "e.p."))
    {
        return AlbumReleaseKind::Ep;
    }
    if types.iter().any(|value| value == "single") {
        return AlbumReleaseKind::Single;
    }

    AlbumReleaseKind::Other
}

pub fn album_release_kind_label(album: &AlbumView) -> &'static str {
    album_release_kind(album).detail_label()
}

impl AlbumReleaseKind {
    fn detail_label(self) -> &'static str {
        match self {
            AlbumReleaseKind::Album => msgid("Album"),
            AlbumReleaseKind::Ep => msgid("EP"),
            AlbumReleaseKind::Single => msgid("Single"),
            AlbumReleaseKind::Collection => msgid("Collection"),
            AlbumReleaseKind::Other => msgid("Release"),
        }
    }
}
