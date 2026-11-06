use super::records::AndroidDetailLink;
use super::*;

impl AndroidLibrary {
    /// What a detail page's header shows of `header`.
    pub(super) async fn detail_summary(
        &self,
        view: &AndroidView,
        header: library::Header,
    ) -> Result<AndroidDetailSummary, AndroidError> {
        use library::{Header, PlaylistListRow, TagKind};
        let summary = |row: Row, artwork_bindings, label: &str, links| AndroidDetailSummary {
            row: row.into(),
            artwork_bindings,
            artist_text: String::new(),
            artist_links: Vec::new(),
            label: label.into(),
            links,
            favorite_total: view.view.favorites_len().unwrap_or(0) as u64,
        };
        let (uri, album_artist) = detail_identity(view.view.route()).unwrap_or_default();
        let link_settings = self.settings.load().application.external_site_links.clone();
        let configuration = library::source_entity_parts(uri)
            .and_then(|(source_id, _, _)| self.source.configuration(&source_id));
        // A local folder link resolves when the page opens; without a folder it goes.
        let local = link_settings.enabled
            && link_settings.server
            && configuration
                .as_ref()
                .is_some_and(|source| source.is_local());
        let folder_uri = if local {
            self.source
                .collection_folder_uri(uri.to_owned(), album_artist)
                .recv()
                .await
                .map_err(error)?
                .ok()
        } else {
            None
        };
        let links = |links: Vec<rufin_core::detail_links::DetailLink>| {
            links
                .into_iter()
                .filter_map(|mut link| {
                    if local && link.url.is_none() {
                        link.url = Some(folder_uri.clone()?);
                    }
                    Some(link.into())
                })
                .collect::<Vec<AndroidDetailLink>>()
        };
        Ok(match header {
            Header::Playlist(playlist) => {
                let links = playlist
                    .source_id
                    .as_ref()
                    .and_then(|id| {
                        self.source
                            .configuration(&sources::SourceId::new(id.clone()))
                    })
                    .and_then(|source| {
                        rufin_core::runtime::source::source_kind_icon_name(&source.kind).map(
                            |icon| AndroidDetailLink {
                                title: source.name.clone(),
                                url: source.detail_web_url("playlist", &playlist.object_id).ok(),
                                icon_id: icon.to_owned(),
                            },
                        )
                    })
                    .into_iter()
                    .collect();
                let artwork = playlist.artwork_binding.iter().cloned().collect();
                summary(
                    Row::Playlist(PlaylistListRow::Playlist(*playlist)),
                    artwork,
                    "Playlist",
                    links,
                )
            }
            Header::Smart(smart) => {
                let artwork = smart.artwork_binding.iter().cloned().collect();
                summary(
                    Row::Playlist(PlaylistListRow::SmartPlaylist(*smart)),
                    artwork,
                    "Playlist",
                    Vec::new(),
                )
            }
            Header::Tag(tag) => {
                let (artwork, label) = match (&tag.artwork_binding, tag.kind) {
                    (Some(binding), TagKind::Genre) => (vec![binding.clone()], "Genre"),
                    (None, TagKind::Genre) => (tag.representative_artwork.clone(), "Genre"),
                    (_, TagKind::Mood) => (tag.representative_artwork.clone(), "Mood"),
                };
                summary(Row::Tag(tag), artwork, label, Vec::new())
            }
            Header::Album(album) => {
                let credited = rufin_core::route::detail_links::album_artist_links(&album);
                let links = links(rufin_core::detail_links::album_external_links(
                    &link_settings,
                    configuration.as_ref(),
                    &album,
                ));
                let label = rufin_core::settings::presentation::album_release_title(
                    album.row.release_class,
                );
                AndroidDetailSummary {
                    artist_text: credited.display_text().to_string(),
                    artist_links: AndroidArtistLink::from_detail_links(&credited)?,
                    ..summary(
                        Row::Album(album.row.clone()),
                        album.row.artwork_binding.iter().cloned().collect(),
                        label,
                        links,
                    )
                }
            }
            Header::Artist(artist) => {
                let links = links(rufin_core::detail_links::artist_external_links(
                    &link_settings,
                    configuration.as_ref(),
                    &artist,
                ));
                let artwork = artist.row.artwork_binding.iter().cloned().collect();
                summary(Row::Artist(artist.row), artwork, "Artist", links)
            }
        })
    }
}
