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

    pub async fn artist_release_groups(
        self: Arc<Self>,
    ) -> Result<Vec<AndroidArtistReleaseGroup>, AndroidError> {
        let Some((uri, album_artist)) = self.artist_release_identity() else {
            return Ok(Vec::new());
        };
        let route = if album_artist {
            Route::AlbumArtistDiscography(uri.to_string())
        } else {
            Route::ArtistDiscography(uri.to_string())
        };
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let counts = self.release_counts().await?;
                let mut index = 0;
                Ok(ARTIST_RELEASE_GROUPS
                    .iter()
                    .zip(counts)
                    .map(|((id, title), total)| {
                        let group = AndroidArtistReleaseGroup {
                            id: (*id).into(),
                            title: localization::tr(title),
                            total: total.max(0) as u64,
                            index,
                            route: route.clone(),
                        };
                        index += group.total;
                        group
                    })
                    .collect())
            })
            .await
            .map_err(error)?
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

    pub async fn detail_summary(
        self: Arc<Self>,
    ) -> Result<Option<AndroidDetailSummary>, AndroidError> {
        if self.detail_identity().is_none()
            && !matches!(
                self.route,
                Route::PlaylistDetail(_)
                    | Route::SmartPlaylistDetail(_)
                    | Route::GenreDetail(_)
                    | Route::MoodDetail(_)
            )
        {
            return Ok(None);
        }
        let identity = self
            .detail_identity()
            .map(|(uri, album_artist)| (uri.to_string(), album_artist));
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let db = &self.database;
                let cancel = ReadCancellation::new();
                let folder = self
                    .selected
                    .as_ref()
                    .and_then(|selected| selected.music_folder_key);
                let named = match self.route {
                    Route::PlaylistDetail(key) => db
                        .playlist_rows(&[key], &cancel)
                        .await
                        .map_err(error)?
                        .pop()
                        .map(|row| {
                            let artwork = row.artwork_binding.iter().cloned().collect::<Vec<_>>();
                            let artwork = if artwork.is_empty() {
                                row.representative_artwork.clone()
                            } else {
                                artwork
                            };
                            (AndroidBrowseRow::from(row), artwork, "Playlist")
                        }),
                    Route::SmartPlaylistDetail(key) => db
                        .smart_playlist_rows(
                            Some(self.selected().source_key),
                            &[key],
                            folder,
                            self.now,
                            &cancel,
                        )
                        .await
                        .map_err(error)?
                        .pop()
                        .map(|row| {
                            let artwork = row.artwork_bindings.clone();
                            let current = row.definition.current;
                            let mut projected = AndroidBrowseRow::from(row);
                            if current {
                                projected.context_source_id =
                                    Some(self.selected().source_id.to_string());
                            }
                            (projected, artwork, "Playlist")
                        }),
                    Route::GenreDetail(key) => db
                        .genre_detail(self.selected().source_key, key, folder, &cancel)
                        .await
                        .map_err(error)?
                        .map(|row| {
                            let artwork = row.artwork_binding.iter().cloned().collect::<Vec<_>>();
                            let artwork = if artwork.is_empty() {
                                row.representative_artwork.clone()
                            } else {
                                artwork
                            };
                            (AndroidBrowseRow::from(row), artwork, "Genre")
                        }),
                    Route::MoodDetail(key) => db
                        .mood_detail(self.selected().source_key, key, folder, &cancel)
                        .await
                        .map_err(error)?
                        .map(|row| {
                            let artwork = row.representative_artwork.clone();
                            (AndroidBrowseRow::from(row), artwork, "Mood")
                        }),
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
                                rufin_core::runtime::source::source_kind_icon_name(&source.kind)
                                    .map(|icon| AndroidDetailLink {
                                        title: source.name.clone(),
                                        url: source
                                            .detail_web_url("playlist", &playlist.object_id)
                                            .ok(),
                                        icon_id: icon.to_owned(),
                                    })
                            })
                            .into_iter()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let show_track_images = self.display.row_fields.iter().any(|field| {
                        matches!(field, LibraryField::Image | LibraryField::TitleMerged)
                    });
                    return Ok(Some(AndroidDetailSummary {
                        row,
                        artwork_bindings,
                        artist_text: String::new(),
                        artist_links: Vec::new(),
                        label: localization::tr(label),
                        mode: self.route.clone(),
                        album_count: 0,
                        links,
                        favorite_route: None,
                        favorite_total: 0,
                        discography_route: None,
                        tracks_route: Some(self.route.clone()),
                        show_track_images,
                        show_track_header: self.display.display.show_header,
                    }));
                }
                let Some((uri, album_artist)) = identity else {
                    return Ok(None);
                };
                let configuration = library::source_entity_parts(&uri)
                    .and_then(|(source_id, _, _)| self.source.configuration(&source_id));
                let link_settings = self.settings.load().external_site_links;
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
                if matches!(self.route, Route::AlbumDetail(_)) {
                    let Some((album, _)) =
                        self.request().album(db, &cancel).await.map_err(error)?
                    else {
                        return Ok(None);
                    };
                    let links = present_links(rufin_core::detail_links::album_external_links(
                        &link_settings,
                        configuration.as_ref(),
                        &album,
                    ));
                    let show_track_images = self.display.row_fields.iter().any(|field| {
                        matches!(field, LibraryField::Image | LibraryField::TitleMerged)
                    });
                    let credited = rufin_core::route::detail_links::album_artist_links(&album);
                    let artist_text = credited.display_text().to_string();
                    let artist_links = AndroidArtistLink::from_detail_links(&credited)?;
                    let artwork_bindings = album.artwork_binding.iter().cloned().collect();
                    let mut row: AndroidBrowseRow = album.into();
                    row.context_source_id =
                        library::source_entity_parts(&uri).map(|(source, _, _)| source.to_string());
                    return Ok(Some(AndroidDetailSummary {
                        artwork_bindings,
                        row,
                        artist_text,
                        artist_links,
                        label: localization::tr("Album"),
                        mode: self.route.clone(),
                        album_count: 1,
                        links,
                        favorite_route: None,
                        favorite_total: 0,
                        discography_route: None,
                        tracks_route: Some(Route::AlbumDetail(uri)),
                        show_track_images,
                        show_track_header: self.display.display.show_header,
                    }));
                }
                let request = self.request();
                let Some(releases) = request.artist(db, &cancel).await.map_err(error)? else {
                    return Ok(None);
                };
                let artist = db
                    .artist_rows(
                        releases.source,
                        &[releases.artist],
                        releases.album_artist,
                        folder,
                        &cancel,
                    )
                    .await
                    .map_err(error)?
                    .pop()
                    .ok_or_else(|| error("Artist is no longer available"))?;
                let favorites = BrowseRequest {
                    filter: "",
                    favorites_only: true,
                    downloaded_only: false,
                    ..request
                }
                .artist_songs(&releases);
                let favorite_total = db
                    .song_count(&favorites, &cancel)
                    .await
                    .map_err(error)?
                    .max(0) as u64;
                let links = present_links(rufin_core::detail_links::artist_external_links(
                    &link_settings,
                    configuration.as_ref(),
                    &artist,
                ));
                let album_count = artist.album_count.max(0) as u64;
                let artwork_bindings = artist.artwork_binding.iter().cloned().collect();
                let mut row = artist_row(artist, album_artist);
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
                let show_track_images =
                    self.artist_tracks.row_fields.iter().any(|field| {
                        matches!(field, LibraryField::Image | LibraryField::TitleMerged)
                    });
                Ok(Some(AndroidDetailSummary {
                    artwork_bindings,
                    row,
                    artist_text: String::new(),
                    artist_links: Vec::new(),
                    label: localization::tr("Artist"),
                    mode: self.route.clone(),
                    album_count,
                    links,
                    favorite_route: Some(favorite_route),
                    favorite_total,
                    discography_route: Some(discography_route),
                    tracks_route: Some(tracks_route),
                    show_track_images,
                    show_track_header: self.artist_tracks.display.show_header,
                }))
            })
            .await
            .map_err(error)?
    }
}
