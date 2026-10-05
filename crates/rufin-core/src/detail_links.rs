use library::{AlbumRow, ArtistRow};
use localization::msgid;

use crate::settings::ExternalSiteLinkSettings;

pub struct DetailLink {
    pub title: &'static str,
    pub icon_name: &'static str,
    pub url: Option<String>,
}

pub fn source_link(source: &sources::SourceConfiguration, media_uri: &str) -> Option<DetailLink> {
    let (_, kind, object_id) = library::source_entity_parts(media_uri)?;
    let folder = matches!(source.kind.as_str(), "local" | "webdav" | "smb");
    let title = match source.kind.as_str() {
        "local" | "webdav" | "smb" => msgid("Open Folder"),
        "jellyfin" => msgid("Open on Jellyfin"),
        "emby" => msgid("Open on Emby"),
        "plex" => msgid("Open on Plex"),
        "navidrome" => msgid("Open on Navidrome"),
        "subsonic" => msgid("Open on server"),
        _ => return None,
    };
    Some(DetailLink {
        title,
        icon_name: if folder {
            "rufin-document-open-symbolic"
        } else {
            crate::runtime::source::source_kind_icon_name(&source.kind)?
        },
        url: if folder {
            None
        } else {
            Some(source.detail_web_url(&kind, &object_id).ok()?)
        },
    })
}

pub fn album_external_links(
    settings: &ExternalSiteLinkSettings,
    source: Option<&sources::SourceConfiguration>,
    album: &AlbumRow,
) -> Vec<DetailLink> {
    if !settings.enabled {
        return Vec::new();
    }
    let mut links = Vec::new();
    if settings.lastfm
        && let Some(url) = lastfm_album_url(&album.display_artist, &album.title)
    {
        links.push(DetailLink {
            title: msgid("Open on Last.fm"),
            icon_name: "io.github.screwys.Rufin.external.lastfm",
            url: Some(url),
        });
    }
    if settings.musicbrainz
        && let Some(url) = musicbrainz_album_url(album)
    {
        links.push(DetailLink {
            title: msgid("Open on MusicBrainz"),
            icon_name: "io.github.screwys.Rufin.external.musicbrainz",
            url: Some(url),
        });
    }
    if settings.server
        && let Some(source) = source
        && let Some(link) = source_link(source, &album.media_uri)
    {
        links.push(link);
    }
    links
}

pub fn artist_external_links(
    settings: &ExternalSiteLinkSettings,
    source: Option<&sources::SourceConfiguration>,
    artist: &ArtistRow,
) -> Vec<DetailLink> {
    if !settings.enabled {
        return Vec::new();
    }
    let mut links = Vec::new();
    if settings.lastfm
        && let Some(url) = lastfm_artist_url(&artist.name)
    {
        links.push(DetailLink {
            title: msgid("Open on Last.fm"),
            icon_name: "io.github.screwys.Rufin.external.lastfm",
            url: Some(url),
        });
    }
    if settings.musicbrainz
        && let Some(url) = musicbrainz_artist_url(artist)
    {
        links.push(DetailLink {
            title: msgid("Open on MusicBrainz"),
            icon_name: "io.github.screwys.Rufin.external.musicbrainz",
            url: Some(url),
        });
    }
    if settings.server
        && let Some(source) = source
        && let Some(link) = source_link(source, &artist.media_uri)
    {
        links.push(link);
    }
    links
}

fn lastfm_album_url(artist: &str, album: &str) -> Option<String> {
    let artist = clean_url_label(artist)?;
    let album = clean_url_label(album)?;
    Some(format!(
        "https://www.last.fm/music/{}/{}",
        percent_encode_path_segment(artist),
        percent_encode_path_segment(album)
    ))
}

fn lastfm_artist_url(artist: &str) -> Option<String> {
    let artist = clean_url_label(artist)?;
    Some(format!(
        "https://www.last.fm/music/{}",
        percent_encode_path_segment(artist)
    ))
}

fn musicbrainz_album_url(album: &AlbumRow) -> Option<String> {
    if let Some(group_id) = album
        .musicbrainz_release_group_id
        .as_deref()
        .and_then(clean_url_label)
    {
        return Some(format!("https://musicbrainz.org/release-group/{group_id}"));
    }
    let release_id = album
        .musicbrainz_release_id
        .as_deref()
        .and_then(clean_url_label)?;
    Some(format!("https://musicbrainz.org/release/{release_id}"))
}

fn musicbrainz_artist_url(artist: &ArtistRow) -> Option<String> {
    let artist_id = artist
        .musicbrainz_artist_id
        .as_deref()
        .and_then(clean_url_label)?;
    Some(format!("https://musicbrainz.org/artist/{artist_id}"))
}

fn clean_url_label(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}
