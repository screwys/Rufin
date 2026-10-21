use super::records::AndroidDetailLink;
use super::*;

#[uniffi::export]
impl AndroidBrowseQuery {
    pub fn detail_track_fields(&self) -> Vec<AndroidSortField> {
        self.selected_track_fields()
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
            let opened = self.opened(&cancel).await.map_err(error)?;
            let OpenedCollection::Detail(Some(detail)) = opened else {
                let rows = self
                    .rows(
                        0,
                        if matches!(
                            opened,
                            OpenedCollection::Home(_) | OpenedCollection::Search(_)
                        ) {
                            usize::MAX
                        } else {
                            64
                        },
                        &cancel,
                    )
                    .await
                    .map_err(error)?;
                let count = if matches!(opened, OpenedCollection::Home(_)) {
                    rows.len()
                } else {
                    self.opened_count(opened)
                };
                return Ok(AndroidBrowsePage {
                    count: count as u64,
                    rows,
                    summary: None,
                    groups: Vec::new(),
                    favorite_rows: Vec::new(),
                });
            };
            let detail = detail.clone();
            let mut groups = Vec::new();
            let mut favorite_rows = Vec::new();
            let mut favorite_total = 0;
            let (count, rows) = match detail.content {
                rufin_core::browse::DetailContent::Tracks(page) => {
                    let rows = self.member_rows(page.page.rows, page.page.first_row_position);
                    (page.page.count as u64, rows)
                }
                rufin_core::browse::DetailContent::Releases { page, favorites } => {
                    let (_, album_artist) =
                        self.artist_release_identity().expect("artist releases");
                    let (uri, _) = self.artist_release_identity().unwrap();
                    let route = if album_artist {
                        Route::AlbumArtistDiscography(uri.to_string())
                    } else {
                        Route::ArtistDiscography(uri.to_string())
                    };
                    let mut first_rows = page.page.rows.into_iter();
                    let mut index = 0;
                    for (class, ((id, title), total)) in
                        ARTIST_RELEASE_GROUPS.iter().zip(page.counts).enumerate()
                    {
                        let rows = first_rows
                            .by_ref()
                            .take(total)
                            .map(|album| release_row(album, class))
                            .collect();
                        groups.push(AndroidArtistReleaseGroup {
                            id: (*id).into(),
                            title: (*title).into(),
                            total: total as u64,
                            index,
                            route: route.clone(),
                            first_rows: rows,
                        });
                        index += total as u64;
                    }
                    if let Some(page) = favorites {
                        favorite_total = page.page.count as u64;
                        favorite_rows =
                            self.member_rows(page.page.rows, page.page.first_row_position);
                    }
                    (index, Vec::new())
                }
            };
            let summary = if embedded {
                None
            } else {
                Some(self.detail_summary(detail.summary, favorite_total).await?)
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
        summary: rufin_core::browse::DetailSummary,
        favorite_total: u64,
    ) -> Result<AndroidDetailSummary, AndroidError> {
        use rufin_core::browse::DetailSummary;
        let db = &self.database;
        let folder = self
            .selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let named = match &summary {
            DetailSummary::Playlist(row) => Some((
                AndroidBrowseRow::from(row.as_ref().clone()),
                row.artwork_binding.iter().cloned().collect(),
                "Playlist",
            )),
            DetailSummary::SmartPlaylist(row) => {
                let mut projected = AndroidBrowseRow::from(row.as_ref().clone());
                if row.definition.current {
                    projected.context_source_id = self
                        .selected
                        .as_ref()
                        .map(|selected| selected.source_id.to_string());
                }
                Some((
                    projected,
                    row.artwork_binding.iter().cloned().collect(),
                    "Playlist",
                ))
            }
            DetailSummary::Genre(row) => Some((
                AndroidBrowseRow::from(row.as_ref().clone()),
                row.artwork_binding
                    .as_ref()
                    .map(std::slice::from_ref)
                    .unwrap_or(&row.representative_artwork)
                    .to_vec(),
                "Genre",
            )),
            DetailSummary::Mood(row) => Some((
                AndroidBrowseRow::from(row.as_ref().clone()),
                row.representative_artwork.clone(),
                "Mood",
            )),
            _ => None,
        };
        if let Some((mut row, artwork_bindings, label)) = named {
            let configuration = row.context_source_id.as_ref().and_then(|id| {
                self.source
                    .configuration(&sources::SourceId::new(id.clone()))
            });
            row.context_source_name = configuration
                .as_ref()
                .map(|source| source.name.clone())
                .unwrap_or_default();
            let links = if let AndroidBrowseItem::Playlist { row: playlist } = &row.item {
                configuration
                    .as_ref()
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
                row,
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
            match db
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
        if let DetailSummary::Album(album) = summary {
            let album = *album;
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
            let artwork_bindings = album.artwork_binding.iter().cloned().collect();
            let mut row: AndroidBrowseRow = album.into();
            row.context_source_id =
                library::source_entity_parts(&uri).map(|(source, _, _)| source.to_string());
            return Ok(AndroidDetailSummary {
                artwork_bindings,
                row,
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
        let DetailSummary::Artist(artist) = summary else {
            unreachable!("artist detail")
        };
        let links = present_links(rufin_core::detail_links::artist_external_links(
            &link_settings,
            configuration.as_ref(),
            &artist,
        ));
        let album_count = artist.summary.album_count.max(0) as u64;
        let artwork_bindings = artist.artwork_binding.iter().cloned().collect();
        let mut row = artist_row(*artist, album_artist);
        row.context_source_id =
            library::source_entity_parts(&uri).map(|(source, _, _)| source.to_string());
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
