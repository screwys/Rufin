use super::records::AndroidDetailLink;
use super::*;

#[uniffi::export]
impl AndroidBrowseQuery {
    /// The fields the page's rows show, by title.
    pub fn fields(&self) -> Vec<AndroidSortField> {
        self.shown_fields()
            .iter()
            .map(|field| AndroidSortField {
                id: *field,
                title: localization::tr(rufin_core::settings::presentation::library_field_title(
                    *field,
                )),
            })
            .collect()
    }

    pub async fn detail_source_uri(&self) -> Result<String, AndroidError> {
        let (uri, album_artist) = self
            .detail_identity()
            .ok_or_else(|| error("Choose an album or artist"))?;
        self.source
            .collection_folder_uri(
                uri.to_string(),
                album_artist,
                self.selected().music_folder_key,
            )
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn initial_page(
        self: Arc<Self>,
        embedded: bool,
    ) -> Result<AndroidBrowsePage, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let opened = self.open(&cancel).await.map_err(error)?;
            let view = &opened.view;
            if let Some(rows) = self.home_rows(view) {
                let rows = rows.to_vec();
                let _ = self.view.set(opened.view);
                return Ok(AndroidBrowsePage {
                    count: rows.len() as u64,
                    rows,
                    summary: None,
                    groups: Vec::new(),
                    favorite_rows: Vec::new(),
                });
            }
            let rows = self.browse_rows(view, opened.rows, opened.first);
            let mut groups = Vec::new();
            if let Some((counts, _)) = view.release_counts() {
                let mut index = 0;
                for ((id, title), total) in ARTIST_RELEASE_GROUPS.iter().zip(counts) {
                    groups.push(AndroidArtistReleaseGroup {
                        id: (*id).into(),
                        title: (*title).into(),
                        total: total as u64,
                        index,
                    });
                    index += total as u64;
                }
            }
            let favorite_rows = opened
                .favorites
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(index, row)| Self::shown_row(row, self.favorite_fields(), index))
                .collect();
            let favorite_total = view.favorites_len().unwrap_or(0) as u64;
            let count = view.len() as u64;
            let header = opened.header.filter(|_| !view.is_missing() && !embedded);
            let _ = self.view.set(opened.view);
            let summary = match header {
                Some(header) => Some(self.detail_summary(header, favorite_total).await?),
                None => None,
            };
            Ok(AndroidBrowsePage {
                count,
                rows,
                summary,
                groups,
                favorite_rows,
            })
        })
        .await
    }
}

impl AndroidBrowseQuery {
    async fn detail_summary(
        &self,
        header: library::Header,
        favorite_total: u64,
    ) -> Result<AndroidDetailSummary, AndroidError> {
        use library::{Header, PlaylistListRow, TagKind};
        let catalog = &self.catalog;
        let folder = self
            .selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let named = match &header {
            Header::Playlist(row) => Some((
                row.artwork_binding.iter().cloned().collect(),
                "Playlist",
                Row::Playlist(PlaylistListRow::Playlist(row.as_ref().clone())),
            )),
            Header::Smart(row) => Some((
                row.artwork_binding.iter().cloned().collect(),
                "Playlist",
                Row::Playlist(PlaylistListRow::SmartPlaylist(row.as_ref().clone())),
            )),
            Header::Tag(row) => Some(if row.kind == TagKind::Genre {
                (
                    row.artwork_binding
                        .as_ref()
                        .map(std::slice::from_ref)
                        .unwrap_or(&row.representative_artwork)
                        .to_vec(),
                    "Genre",
                    Row::Tag(row.clone()),
                )
            } else {
                (
                    row.representative_artwork.clone(),
                    "Mood",
                    Row::Tag(row.clone()),
                )
            }),
            _ => None,
        };
        if let Some((artwork_bindings, label, row)) = named {
            let links = if let Header::Playlist(playlist) = &header {
                playlist
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
                    .collect()
            } else {
                Vec::new()
            };
            let show_track_images = self
                .display
                .row_fields
                .iter()
                .any(|field| matches!(field, LibraryField::Image | LibraryField::TitleMerged));
            return Ok(AndroidDetailSummary {
                row: row.into(),
                artwork_bindings,
                artist_text: String::new(),
                artist_links: Vec::new(),
                label: label.into(),
                mode: self.route.clone(),
                album_count: 0,
                links,
                favorite_route: None,
                favorite_total: 0,
                discography_route: None,
                tracks_route: Some(self.route.clone()),
                show_track_images,
                show_track_header: self.display.display.show_header,
            });
        }
        let (uri, album_artist) = self.detail_identity().expect("album or artist detail");
        let uri = uri.to_string();
        let configuration = library::source_entity_parts(&uri)
            .and_then(|(source_id, _, _)| self.source.configuration(&source_id));
        let link_settings = self.settings.load().application.external_site_links;
        let local_folder_link = link_settings.enabled
            && link_settings.server
            && configuration
                .as_ref()
                .is_some_and(|source| source.is_local());
        let folder_uri = if local_folder_link {
            match catalog
                .collection_source_path_bounds(&uri, album_artist, folder)
                .await
                .map_err(error)?
            {
                Some((first, last)) => configuration
                    .as_ref()
                    .unwrap()
                    .detail_folder_uri(&first, &last)
                    .map_err(error)?,
                None => None,
            }
        } else {
            None
        };
        let present_links = |links: Vec<rufin_core::detail_links::DetailLink>| {
            links
                .into_iter()
                .filter_map(|mut link| {
                    if local_folder_link && link.url.is_none() {
                        link.url = Some(folder_uri.clone()?);
                    }
                    Some(link.into())
                })
                .collect::<Vec<AndroidDetailLink>>()
        };
        if let Header::Album(album) = header {
            let links = present_links(rufin_core::detail_links::album_external_links(
                &link_settings,
                configuration.as_ref(),
                &album,
            ));
            let show_track_images = self
                .display
                .row_fields
                .iter()
                .any(|field| matches!(field, LibraryField::Image | LibraryField::TitleMerged));
            let credited = rufin_core::route::detail_links::album_artist_links(&album);
            let artist_text = credited.display_text().to_string();
            let artist_links = AndroidArtistLink::from_detail_links(&credited)?;
            let artwork_bindings = album.row.artwork_binding.iter().cloned().collect();
            return Ok(AndroidDetailSummary {
                artwork_bindings,
                row: Row::Album(album.row).into(),
                artist_text,
                artist_links,
                label: "Album".into(),
                mode: self.route.clone(),
                album_count: 1,
                links,
                favorite_route: None,
                favorite_total: 0,
                discography_route: None,
                tracks_route: Some(Route::AlbumDetail(uri)),
                show_track_images,
                show_track_header: self.display.display.show_header,
            });
        }
        let Header::Artist(artist) = header else {
            unreachable!("artist detail")
        };
        let links = present_links(rufin_core::detail_links::artist_external_links(
            &link_settings,
            configuration.as_ref(),
            &artist,
        ));
        let album_count = artist.row.summary.album_count.max(0) as u64;
        let artwork_bindings = artist.row.artwork_binding.iter().cloned().collect();
        let row = Row::Artist(artist.row).into();
        let favorite_route = if album_artist {
            Route::AlbumArtistFavoriteTracks(uri.clone())
        } else {
            Route::ArtistFavoriteTracks(uri.clone())
        };
        let discography_route = if album_artist {
            Route::AlbumArtistDiscography(uri.clone())
        } else {
            Route::ArtistDiscography(uri.clone())
        };
        let tracks_route = if album_artist {
            Route::AlbumArtistTracks(uri)
        } else {
            Route::ArtistTracks(uri)
        };
        let show_track_images = self
            .artist_tracks
            .row_fields
            .iter()
            .any(|field| matches!(field, LibraryField::Image | LibraryField::TitleMerged));
        Ok(AndroidDetailSummary {
            artwork_bindings,
            row,
            artist_text: String::new(),
            artist_links: Vec::new(),
            label: "Artist".into(),
            mode: self.route.clone(),
            album_count,
            links,
            favorite_route: Some(favorite_route),
            favorite_total,
            discography_route: Some(discography_route),
            tracks_route: Some(tracks_route),
            show_track_images,
            show_track_header: self.artist_tracks.display.show_header,
        })
    }
}
