//! Links that share a track, album, artist or playlist. Navidrome and OpenSubsonic servers that
//! let the user share make a public link; Jellyfin, Emby and Plex link to the item in their web
//! client, which needs a login there. Other sources have nothing to link to.

use async_channel::Receiver;
use library::{PlaylistKey, PlaylistListRow, ReadCancellation, Row, SourceKind};
use localization::msgid;
use sources::{SourceConfiguration, SourceId};

use crate::source::SourceOwner;
use crate::string_error;

/// What a share links to.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ShareTarget {
    /// A track, album or artist, by media URI.
    Media(String),
    Playlist(PlaylistKey),
}

/// A share a menu offers: what it links to, and the dialog Share opens first when the server
/// makes the link.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Share {
    pub target: ShareTarget,
    pub dialog: Option<ShareDialog>,
}

/// The dialog before the server makes a link: its expiry and description, and whether it allows
/// downloads where the server lets Rufin say so.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ShareDialog {
    pub downloadable: bool,
}

/// What the dialog chose.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[serde(default)]
pub struct ShareOptions {
    pub expiry: ShareExpiry,
    pub description: String,
    pub downloadable: bool,
}

/// How long a server-made link lasts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ShareExpiry {
    #[default]
    ServerDefault,
    Day,
    Week,
    Month,
    Year,
}

impl ShareExpiry {
    fn millis(self) -> Option<u64> {
        let days = match self {
            Self::ServerDefault => return None,
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        u64::try_from(now.as_millis())
            .ok()
            .map(|now| now + days * 86_400_000)
    }
}

enum Link {
    /// The server makes a public link.
    Created,
    /// The item's page in the server's web client.
    WebClient,
}

fn link(source: &SourceConfiguration) -> Option<Link> {
    match source.source_kind()? {
        SourceKind::Navidrome | SourceKind::Subsonic => Some(Link::Created),
        SourceKind::Jellyfin | SourceKind::Emby | SourceKind::Plex => Some(Link::WebClient),
        SourceKind::Local | SourceKind::Webdav | SourceKind::Smb | SourceKind::Peer => None,
    }
}

fn offer(owner: &SourceOwner, source_id: &SourceId, target: ShareTarget) -> Option<Share> {
    let dialog = match link(&owner.configuration(source_id)?)? {
        Link::Created => {
            let source = owner
                .client(source_id)
                .ok()
                .filter(|source| source.shares_links())?;
            Some(ShareDialog {
                downloadable: source.supports_share_downloads(),
            })
        }
        Link::WebClient => None,
    };
    Some(Share { target, dialog })
}

/// The share `row` offers, when its source has links to share.
pub fn offered(owner: &SourceOwner, row: &Row) -> Option<Share> {
    match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => media(owner, &track.media_uri),
        Row::Album(album) | Row::AlbumHeading(album) => media(owner, &album.media_uri),
        Row::Artist(artist) => media(owner, &artist.media_uri),
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => offer(
            owner,
            &SourceId::new(playlist.source_id.clone()?),
            ShareTarget::Playlist(playlist.playlist_key),
        ),
        _ => None,
    }
}

/// The share of the track, album or artist at `uri`, when its source has links to share.
pub fn media(owner: &SourceOwner, uri: &str) -> Option<Share> {
    let (source_id, ..) = library::source_entity_parts(uri)?;
    offer(owner, &source_id, ShareTarget::Media(uri.to_owned()))
}

/// The link that shares `target`; a link the server makes holds `options`.
pub fn share(
    owner: &SourceOwner,
    target: ShareTarget,
    options: ShareOptions,
) -> Receiver<Result<String, String>> {
    owner.reply(move |owner, catalog| async move {
        let (source_id, kind, object_id) = match target {
            ShareTarget::Media(uri) => library::source_entity_parts(&uri),
            ShareTarget::Playlist(key) => catalog
                .playlists(&[key], &ReadCancellation::new())
                .await
                .map_err(string_error)?
                .pop()
                .and_then(|playlist| {
                    Some((
                        SourceId::new(playlist.source_id?),
                        "playlist".to_owned(),
                        playlist.object_id,
                    ))
                }),
        }
        .ok_or(msgid("This item has no link to share"))?;
        let source = owner
            .configuration(&source_id)
            .ok_or("The connection no longer exists")?;
        match link(&source).ok_or(msgid("This item has no link to share"))? {
            Link::Created => owner
                .client(&source_id)?
                .create_share(
                    &object_id,
                    &sources::NewShare {
                        description: options.description.trim().to_owned(),
                        expires_millis: options.expiry.millis(),
                        downloadable: options.downloadable,
                    },
                )
                .await
                .map_err(string_error),
            Link::WebClient => source
                .detail_web_url(&kind, &object_id)
                .map_err(string_error),
        }
    })
}
