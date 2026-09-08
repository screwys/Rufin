//! Destinations in a source's web client or file manager.

use std::path::Path;

use url::Url;

use crate::{SourceConfiguration, SourceError, SourceResult};

impl SourceConfiguration {
    pub fn detail_web_url(&self, kind: &str, object_id: &str) -> SourceResult<String> {
        let id = entity_id(&self.kind, kind, object_id)?;
        let url = match self.kind.as_str() {
            "jellyfin" | "emby" => {
                let config =
                    crate::jellyfin_emby::JellyfinEmbySourceConfig::from_configuration(self)?;
                let mut base = web_base(&config.base_url)?;
                if self.kind == "emby" {
                    let path = base.path().trim_end_matches('/');
                    let path = path.strip_suffix("/emby").unwrap_or(path).to_owned();
                    base.set_path(&format!("{path}/"));
                }
                let mut url = base.join("web/index.html").map_err(url_error)?;
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                query.append_pair("id", id);
                if let Some(server) = config.server_id.as_deref() {
                    query.append_pair("serverId", server);
                }
                let route = if self.kind == "emby" {
                    "item"
                } else {
                    "details"
                };
                url.set_fragment(Some(&format!("!/{route}?{}", query.finish())));
                url
            }
            "plex" => {
                let config = crate::plex::PlexSourceConfig::from_configuration(self)?;
                let mut url = web_base(&config.base_url)?
                    .join("web/index.html")
                    .map_err(url_error)?;
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("key", &format!("/library/metadata/{id}"))
                    .finish();
                url.set_fragment(Some(&format!(
                    "!/server/{}/details?{query}",
                    encode(&config.server_id)
                )));
                url
            }
            "navidrome" | "subsonic" => {
                let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
                subsonic_url(&web_base(&config.base_url)?, &self.kind, kind, id)?
                    .ok_or(SourceError::NotFound)?
            }
            _ => return Err(SourceError::InvalidRequest("Source has no web client")),
        };
        Ok(url.into())
    }

    pub fn detail_folder_uri(&self, first: &str, last: &str) -> SourceResult<String> {
        if self.kind == "local" {
            let first = Path::new(first).parent().ok_or(SourceError::NotFound)?;
            let last = Path::new(last).parent().ok_or(SourceError::NotFound)?;
            let common = first
                .ancestors()
                .find(|path| last.starts_with(path))
                .ok_or(SourceError::NotFound)?;
            return Url::from_directory_path(common)
                .map(String::from)
                .map_err(|_| SourceError::NotFound);
        }
        crate::file::remote::detail_folder_uri(self, first, last)
    }
}

fn entity_id<'a>(source: &str, kind: &str, id: &'a str) -> SourceResult<&'a str> {
    id.strip_prefix(&format!("{source}:{kind}:"))
        .filter(|id| !id.is_empty() && matches!(kind, "artist" | "album"))
        .ok_or(SourceError::NotFound)
}

fn web_base(base: &str) -> SourceResult<Url> {
    let mut url = Url::parse(base).map_err(url_error)?;
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

fn subsonic_url(base: &Url, server: &str, kind: &str, id: &str) -> SourceResult<Option<Url>> {
    let mut url = web_base(base.as_str())?;
    let path = url.path().trim_end_matches('/').to_owned();
    // These API mount points identify the web app even on older saved connections.
    if let Some(app) = path.strip_suffix("/apps/music/subsonic") {
        url.set_path(&format!("{app}/apps/music/"));
        let id = id.strip_prefix(&format!("{kind}-")).unwrap_or(id);
        url.set_fragment(Some(&format!("/{kind}/{}", encode(id))));
    } else if let Some(root) = path.strip_suffix("/api/subsonic") {
        url.set_path(&format!("{root}/library/{kind}s/{}", encode(id)));
    } else {
        match server.to_ascii_lowercase().as_str() {
            "navidrome" => {
                url = url.join("app/").map_err(url_error)?;
                url.set_fragment(Some(&format!("/{kind}/{}/show", encode(id))));
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(url))
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn url_error(error: url::ParseError) -> SourceError {
    SourceError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn configuration(kind: &str, payload: serde_json::Value) -> SourceConfiguration {
        SourceConfiguration {
            source_id: crate::SourceId::new("server"),
            kind: kind.into(),
            name: "My custom name".into(),
            provider_payload: payload.to_string(),
        }
    }

    #[test]
    fn server_detail_links_keep_mount_paths_and_encode_item_and_server_ids() {
        for (kind, base, route) in [
            ("jellyfin", "https://music.test/proxy/", "details"),
            ("emby", "https://music.test/proxy/emby/", "item"),
        ] {
            let source = configuration(
                kind,
                json!({"version":1,"base_url":base,"server_id":"server & two","user_id":"listener","username":"listener","trust_invalid_cert":false}),
            );
            for entity in ["artist", "album"] {
                assert_eq!(
                    source
                        .detail_web_url(entity, &format!("{kind}:{entity}:42&other=1"))
                        .unwrap(),
                    format!(
                        "https://music.test/proxy/web/index.html#!/{route}?id=42%26other%3D1&serverId=server+%26+two"
                    )
                );
            }
        }
        for base in ["http://localhost:32400", "https://music.test/proxy/"] {
            let source = configuration(
                "plex",
                json!({"version":1,"server_id":"machine","profile_id":"profile","base_url":base,"address_override":null,"local":true,"relay":false,"owned":true,"trust_invalid_cert":false}),
            );
            for entity in ["artist", "album"] {
                assert_eq!(
                    source
                        .detail_web_url(entity, &format!("plex:{entity}:42"))
                        .unwrap(),
                    format!(
                        "{}/web/index.html#!/server/machine/details?key=%2Flibrary%2Fmetadata%2F42",
                        base.trim_end_matches('/')
                    )
                );
            }
        }
        let source = configuration(
            "navidrome",
            json!({"version":1,"base_url":"https://music.test/navidrome/","username":"listener","trust_invalid_cert":false}),
        );
        assert_eq!(
            source
                .detail_web_url("artist", "navidrome:artist:a/b")
                .unwrap(),
            "https://music.test/navidrome/app/#/artist/a%2Fb/show"
        );
    }

    #[test]
    fn subsonic_web_clients_use_their_real_routes_and_id_namespaces() {
        for (base, server, kind, id, expected) in [
            (
                "https://cloud.test/cloud/index.php/apps/music/subsonic/",
                "Nextcloud Music",
                "album",
                "album-42",
                "https://cloud.test/cloud/index.php/apps/music/#/album/42",
            ),
            (
                "https://cloud.test/apps/music/subsonic/",
                "ownCloud Music",
                "artist",
                "artist-17",
                "https://cloud.test/apps/music/#/artist/17",
            ),
            (
                "https://music.test/proxy/api/subsonic/",
                "funkwhale",
                "artist",
                "12",
                "https://music.test/proxy/library/artists/12",
            ),
            (
                "https://music.test/api/subsonic/",
                "funkwhale",
                "album",
                "15",
                "https://music.test/library/albums/15",
            ),
            (
                "https://music.test/nav/",
                "Navidrome",
                "album",
                "42",
                "https://music.test/nav/app/#/album/42/show",
            ),
        ] {
            assert_eq!(
                subsonic_url(&Url::parse(base).unwrap(), server, kind, id)
                    .unwrap()
                    .unwrap()
                    .as_str(),
                expected
            );
        }
        for server in ["airsonic", "gonic", "unknown"] {
            let source = configuration(
                "subsonic",
                json!({"version":1,"base_url":format!("https://music.test/{server}/"),"username":"listener","trust_invalid_cert":false}),
            );
            assert!(source.detail_web_url("album", "subsonic:album:42").is_err());
        }
        for (base, entity, id, expected) in [
            (
                "https://cloud.test/apps/music/subsonic",
                "artist",
                "artist-42",
                "https://cloud.test/apps/music/#/artist/42",
            ),
            (
                "https://music.test/api/subsonic",
                "album",
                "42",
                "https://music.test/library/albums/42",
            ),
        ] {
            let source = configuration(
                "subsonic",
                json!({"version":1,"base_url":base,"username":"listener","trust_invalid_cert":false}),
            );
            assert_eq!(
                source
                    .detail_web_url(entity, &format!("subsonic:{entity}:{id}"))
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn local_detail_folder_uses_native_components_and_the_common_parent() {
        let directory = tempfile::tempdir().unwrap();
        let source = configuration("local", json!({}));
        let first = directory
            .path()
            .join("Artist & Friends/Album/Disc 1/a.flac");
        let last = directory
            .path()
            .join("Artist & Friends/Album/Disc 2/z.flac");
        let uri = source
            .detail_folder_uri(first.to_str().unwrap(), last.to_str().unwrap())
            .unwrap();
        assert_eq!(
            Url::parse(&uri).unwrap().to_file_path().unwrap(),
            directory.path().join("Artist & Friends/Album")
        );
        let sibling = directory.path().join("Artist & Friends/Album Two/z.flac");
        let uri = source
            .detail_folder_uri(first.to_str().unwrap(), sibling.to_str().unwrap())
            .unwrap();
        assert_eq!(
            Url::parse(&uri).unwrap().to_file_path().unwrap(),
            directory.path().join("Artist & Friends")
        );
    }

    #[test]
    fn network_detail_folder_uses_the_current_address_and_preserves_escaped_paths() {
        for (kind, namespace, current, expected) in [
            (
                "webdav",
                "https://old.test/dav/",
                "https://new.test/music/",
                "https://new.test/music/Artist%20One/Album/",
            ),
            (
                "smb",
                "smb://old.test/share/",
                "smb://new.test/music/",
                "smb://new.test/music/Artist%20One/Album/",
            ),
        ] {
            let source = configuration(
                kind,
                json!({"version":1,"namespace_url":namespace,"url":current}),
            );
            // Deserialize the complete settings used by real configured sources.
            let settings = crate::FileSourceSettings {
                url: current.into(),
                alternate_urls: vec![],
                folders: vec![],
                username: String::new(),
                domain: String::new(),
                authentication: crate::FileAuthentication::Anonymous,
                trust_invalid_certificate: false,
                certificate_pem: None,
                require_smb_encryption: false,
            };
            let mut payload = serde_json::to_value(settings).unwrap();
            payload["version"] = json!(1);
            payload["namespace_url"] = json!(namespace);
            let source = SourceConfiguration {
                provider_payload: payload.to_string(),
                ..source
            };
            assert_eq!(
                source
                    .detail_folder_uri(
                        &format!("{namespace}Artist%20One/Album/Disc%201/a.flac"),
                        &format!("{namespace}Artist%20One/Album/Disc%202/z.flac"),
                    )
                    .unwrap(),
                expected
            );
            assert_eq!(
                source
                    .detail_folder_uri(
                        &format!("{namespace}A/a.flac"),
                        &format!("{namespace}Z/z.flac")
                    )
                    .unwrap(),
                current,
            );
        }
    }
}
