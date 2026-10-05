use super::*;
use crate::library_events::favorite_target;

fn route_descriptor(item: SidebarRouteItem) -> AndroidRouteDescriptor {
    let descriptor = sidebar_route_descriptor(item);
    AndroidRouteDescriptor {
        id: descriptor.stable_id.into(),
        title: localization::tr(descriptor.title),
        icon_name: descriptor.icon_name.into(),
        selected_icon_name: descriptor.selected_icon_name().into(),
        route: route_json(descriptor.root_route),
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn target_download_status(
        &self,
        route: Option<String>,
        media_uri: String,
        source_id: Option<String>,
    ) -> Result<crate::downloads::AndroidTargetDownloadStatus, AndroidError> {
        let database = self.database.clone();
        let selected = self.source.selected_library();
        let source_id = source_id.or_else(|| {
            library::source_entity_parts(&media_uri).map(|(source, _, _)| source.to_string())
        });
        let route = route
            .map(|route| serde_json::from_str::<Route>(&route))
            .transpose()
            .map_err(error)?;
        let folder_query = if matches!(route, Some(Route::Folders { .. })) {
            Some(self.browse_query(
                route.clone().unwrap(),
                "tracks".into(),
                String::new(),
                String::new(),
                false,
                false,
                false,
                None,
            )?)
        } else {
            None
        };
        self.runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let source = match source_id {
                    Some(id) => database
                        .source_identity_key(&sources::SourceId::new(id))
                        .await
                        .map_err(error)?,
                    None => selected.as_ref().map(|selected| selected.source_key),
                };
                let (total, downloaded) = match route {
                    None => (
                        1,
                        database
                            .downloaded_media_count(&[media_uri], &cancel)
                            .await
                            .map_err(error)?,
                    ),
                    Some(Route::AlbumDetail(uri)) => database
                        .album_row_by_media_uri(&uri, &cancel)
                        .await
                        .map_err(error)?
                        .map(|row| (row.track_count, row.downloaded_count))
                        .unwrap_or_default(),
                    Some(
                        Route::ArtistDetail(uri)
                        | Route::ArtistDiscography(uri)
                        | Route::ArtistTracks(uri)
                        | Route::ArtistFavoriteTracks(uri),
                    ) => database
                        .artist_row_by_media_uri(&uri, &cancel)
                        .await
                        .map_err(error)?
                        .map(|row| (row.track_count, row.downloaded_count))
                        .unwrap_or_default(),
                    Some(
                        Route::AlbumArtistDetail(uri)
                        | Route::AlbumArtistDiscography(uri)
                        | Route::AlbumArtistTracks(uri)
                        | Route::AlbumArtistFavoriteTracks(uri),
                    ) => {
                        let collection = library::QueueCollection::Artist {
                            media_uri: uri,
                            album_artist: true,
                        };
                        let total = database
                            .collection_tracks_count(&collection, None, "", false, &cancel)
                            .await
                            .map_err(error)?;
                        let downloaded = if let Some(source) = source {
                            database
                                .query_tracks_count(
                                    &library::TrackQuery {
                                        source,
                                        collection: Some(collection),
                                        folder: None,
                                        favorites_only: false,
                                        downloaded_only: true,
                                    },
                                    "",
                                    &cancel,
                                )
                                .await
                                .map_err(error)?
                        } else {
                            0
                        };
                        (total, downloaded)
                    }
                    Some(Route::PlaylistDetail(key)) => database
                        .playlist_rows(&[key], &cancel)
                        .await
                        .map_err(error)?
                        .first()
                        .map(|row| (row.track_count, row.downloaded_count))
                        .unwrap_or_default(),
                    Some(Route::SmartPlaylistDetail(key)) => database
                        .smart_playlist_rows(
                            source,
                            &[key],
                            selected.as_ref().and_then(|source| source.music_folder_key),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_err(error)?
                                .as_secs() as i64,
                            &cancel,
                        )
                        .await
                        .map_err(error)?
                        .first()
                        .map(|row| (row.track_count, row.downloaded_count))
                        .unwrap_or_default(),
                    Some(Route::GenreDetail(key)) => {
                        if let Some(source) = source {
                            database
                                .genre_rows(source, &[key], None, &cancel)
                                .await
                                .map_err(error)?
                                .first()
                                .map(|row| (row.track_count, row.downloaded_count))
                                .unwrap_or_default()
                        } else {
                            (0, 0)
                        }
                    }
                    Some(Route::MoodDetail(key)) => {
                        if let Some(source) = source {
                            database
                                .mood_rows(source, &[key], None, &cancel)
                                .await
                                .map_err(error)?
                                .first()
                                .map(|row| (row.track_count, row.downloaded_count))
                                .unwrap_or_default()
                        } else {
                            (0, 0)
                        }
                    }
                    Some(Route::Folders { .. }) => {
                        let query = folder_query.unwrap();
                        let source = query.selected().source_key;
                        match query.folder_projection().await.map_err(error)? {
                            FolderProjection::Live { candidates, .. } => (
                                database
                                    .live_folder_track_count(source, candidates, "", &cancel)
                                    .await
                                    .map_err(error)?,
                                database
                                    .downloaded_media_count(candidates, &cancel)
                                    .await
                                    .map_err(error)?,
                            ),
                            FolderProjection::Cached(folder) => (
                                database
                                    .track_count(source, *folder, false, "", &cancel)
                                    .await
                                    .map_err(error)?,
                                database
                                    .query_tracks_count(
                                        &library::TrackQuery {
                                            source,
                                            collection: None,
                                            folder: *folder,
                                            favorites_only: false,
                                            downloaded_only: true,
                                        },
                                        "",
                                        &cancel,
                                    )
                                    .await
                                    .map_err(error)?,
                            ),
                        }
                    }
                    _ => (0, 0),
                };
                Ok(crate::downloads::AndroidTargetDownloadStatus {
                    total: total.max(0) as u64,
                    downloaded: downloaded.max(0) as u64,
                })
            })
            .await
            .map_err(error)?
    }

    pub async fn download_target(
        &self,
        route: Option<String>,
        media_uri: String,
        remove: bool,
    ) -> Result<(), AndroidError> {
        if let Some(route) = route.as_deref()
            && matches!(
                serde_json::from_str::<Route>(route).map_err(error)?,
                Route::Folders { .. }
            )
        {
            let media_uris = self.resolve_target_uris(route).await?;
            if remove {
                self.source.remove_download_media(media_uris);
            } else {
                self.source.download_media(
                    downloads::DownloadSubject::for_media_uris(
                        route,
                        Some(&localization::tr("Folder")),
                        &media_uris,
                    ),
                    media_uris,
                );
            }
            return Ok(());
        }
        let target = match route {
            Some(route) => self.playback_target(&route).await?,
            None => rufin_core::playback::PlaybackTarget::Track(media_uri),
        };
        let selected = self.source.selected_library();
        let source = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let result = if remove {
            self.source.remove_download_target(target, source, folder)
        } else {
            self.source.download_target(target, source, folder)
        };
        result.recv().await.map_err(error)?.map_err(error)
    }
}

impl AndroidLibrary {
    pub(super) fn browse_query(
        &self,
        route: Route,
        category: String,
        filter: String,
        sort: String,
        descending: bool,
        favorites_only: bool,
        downloaded_only: bool,
        release_section: Option<ArtistReleaseSection>,
    ) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        let history_current_only = category == "current";
        let category = CollectionCategory::from_name(&category);
        let settings = self.settings.load();
        let display = settings.library_list(library_list_key(&route, category));
        let (sort, descending) = if sort.is_empty() {
            let defaults = settings.library_list(library_list_key(&route, category));
            (defaults.sort_key, defaults.descending)
        } else {
            (
                serde_json::from_value(serde_json::Value::String(sort)).map_err(error)?,
                descending,
            )
        };
        let selected = self.source.selected_library();
        if selected.is_none()
            && !matches!(route, Route::Playlists | Route::PlaylistDetail(_))
            && (!matches!(route, Route::History) || history_current_only)
        {
            return Err(error("No source selected"));
        }
        Ok(Arc::new(AndroidBrowseQuery {
            selected,
            database: self.database.clone(),
            runtime: self.runtime.clone(),
            queue: self.queue.clone(),
            route,
            home_variations: self.source.home_variations(),
            home_snapshot: tokio::sync::OnceCell::new(),
            history_current_only,
            favorites_only,
            downloaded_only,
            category,
            display,
            album_tracks: settings.library_list(LibraryListKey::AlbumDetailTracks),
            artist_tracks: settings.library_list(LibraryListKey::ArtistTracks),
            source: self.source.clone(),
            settings: self.settings.clone(),
            release_section,
            release_counts: tokio::sync::OnceCell::new(),
            folder_projection: tokio::sync::OnceCell::new(),
            filter,
            sort,
            descending,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(error)?
                .as_secs() as i64,
            blocks: settings.home_blocks,
        }))
    }

    pub(super) async fn playback_target(
        &self,
        route: &str,
    ) -> Result<rufin_core::playback::PlaybackTarget, AndroidError> {
        use rufin_core::playback::PlaybackTarget;
        let target = match serde_json::from_str::<Route>(route).map_err(error)? {
            Route::AlbumDetail(uri) => PlaybackTarget::Album(uri),
            Route::ArtistDetail(uri) => PlaybackTarget::Artist(uri),
            Route::AlbumArtistDetail(uri) => PlaybackTarget::AlbumArtist(uri),
            Route::GenreDetail(key) => PlaybackTarget::Genre(key),
            Route::MoodDetail(key) => PlaybackTarget::Mood(key),
            Route::PlaylistDetail(key) => PlaybackTarget::Playlist(key),
            Route::SmartPlaylistDetail(key) => PlaybackTarget::SmartPlaylist(key),
            _ => return Err(error("Choose a collection")),
        };
        Ok(target)
    }
    async fn resolve_target_uris(&self, route: &str) -> Result<Vec<String>, AndroidError> {
        let parsed = serde_json::from_str::<Route>(route).map_err(error)?;
        if matches!(parsed, Route::Folders { .. }) {
            let query = self.browse_query(
                parsed,
                "tracks".into(),
                String::new(),
                String::new(),
                false,
                false,
                false,
                None,
            )?;
            return self
                .runtime
                .spawn(async move { query.folder_target_uris().await })
                .await
                .map_err(error)?;
        }
        let target = self.playback_target(route).await?;
        let selected = self.source.selected_library();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                target
                    .resolve_media_uris(
                        &database,
                        selected.as_ref().map(|selected| selected.source_key),
                        selected
                            .as_ref()
                            .and_then(|selected| selected.music_folder_key),
                    )
                    .await
                    .map_err(error)
            })
            .await
            .map_err(error)?
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub fn music_folders(&self) -> AndroidMusicFolderState {
        let selected = self.source.selected_library();
        let mut choices = vec![AndroidMusicFolder {
            id: None,
            title: localization::tr("All Music"),
        }];
        if let Some(selected) = &selected {
            choices.extend(
                selected
                    .music_folders
                    .iter()
                    .map(|folder| AndroidMusicFolder {
                        id: Some(folder.object_id.clone()),
                        title: folder.name.clone(),
                    }),
            );
        }
        AndroidMusicFolderState {
            selected_id: selected.and_then(|selected| selected.music_folder_object_id),
            choices,
        }
    }

    pub async fn set_music_folder(&self, id: Option<String>) -> Result<(), AndroidError> {
        self.source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?
            .operations
            .set_music_folder(id)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub fn route_kind(&self, route: String) -> Result<String, AndroidError> {
        Ok(core_route_kind(
            &serde_json::from_str::<Route>(&route).map_err(error)?,
        ))
    }
    pub fn artist_release_query(
        &self,
        route: String,
        section_id: String,
        filter: String,
        sort: String,
        descending: bool,
    ) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        let parsed: Route = serde_json::from_str(&route).map_err(error)?;
        if !matches!(
            parsed,
            Route::ArtistDetail(_)
                | Route::ArtistDiscography(_)
                | Route::AlbumArtistDetail(_)
                | Route::AlbumArtistDiscography(_)
        ) {
            return Err(error("Choose an artist"));
        }
        let section = release_section(&section_id)?;
        let route = match parsed {
            Route::ArtistDetail(uri) => Route::ArtistDiscography(uri),
            Route::AlbumArtistDetail(uri) => Route::AlbumArtistDiscography(uri),
            route => route,
        };
        self.browse_query(
            route,
            "albums".into(),
            filter,
            sort,
            descending,
            false,
            false,
            Some(section),
        )
    }
    pub fn set_browse_display(
        &self,
        route: String,
        category: String,
        layout: String,
        size: String,
        grid_spacing: String,
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        let route: Route = serde_json::from_str(&route).map_err(error)?;
        let key = library_list_key(&route, CollectionCategory::from_name(&category));
        let layout = serde_json::from_value(serde_json::Value::String(layout)).map_err(error)?;
        let size = serde_json::from_value(serde_json::Value::String(size)).map_err(error)?;
        let grid_spacing =
            serde_json::from_value(serde_json::Value::String(grid_spacing)).map_err(error)?;
        let effective = self
            .settings
            .update_library_list_settings(key, |settings| {
                settings.layout = layout;
                settings.display.size = size;
                settings.display.grid_spacing = grid_spacing;
            })
            .map_err(error)?
            .unwrap_or_else(|| self.settings.load());
        Ok(browse_display(key, &effective.library_list(key)))
    }
    pub fn set_browse_sort(
        &self,
        route: String,
        category: String,
        sort: String,
        descending: bool,
    ) -> Result<(), AndroidError> {
        let route: Route = serde_json::from_str(&route).map_err(error)?;
        let key = library_list_key(&route, CollectionCategory::from_name(&category));
        let sort = serde_json::from_value(serde_json::Value::String(sort)).map_err(error)?;
        self.settings
            .update_library_list_settings(key, |settings| {
                settings.sort_key = sort;
                settings.descending = descending;
            })
            .map_err(error)?;
        Ok(())
    }

    pub async fn create_target_playlist(
        &self,
        source_id: Option<String>,
        name: String,
        target_route: String,
        public: Option<bool>,
    ) -> Result<Option<String>, AndroidError> {
        let order = self.resolve_target_uris(&target_route).await?;
        self.create_playlist(source_id, name, order, public).await
    }

    pub async fn add_target_to_playlist(
        &self,
        playlist_route: String,
        target_route: String,
        skip_duplicates: bool,
    ) -> Result<u64, AndroidError> {
        let Route::PlaylistDetail(playlist) =
            serde_json::from_str(&playlist_route).map_err(error)?
        else {
            return Err(error("Choose a playlist"));
        };
        let order = self.resolve_target_uris(&target_route).await?;
        rufin_core::playlists::add_playlist_tracks(&self.source, playlist, order, skip_duplicates)
            .recv()
            .await
            .map_err(error)?
            .map(|count| count as u64)
            .map_err(error)
    }

    pub async fn play_target(
        &self,
        route: String,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let parsed = serde_json::from_str::<Route>(&route).map_err(error)?;
        let context_title = context_caption(&parsed, context_title);
        if matches!(parsed, Route::Folders { .. }) {
            let query = self.browse_query(
                parsed,
                "tracks".into(),
                String::new(),
                String::new(),
                false,
                false,
                false,
                None,
            )?;
            let placement = queue_placement(&placement)?;
            return self
                .runtime
                .spawn(async move {
                    query
                        .folder_target_play(placement, shuffled, context_title)
                        .await
                })
                .await
                .map_err(error)?;
        }
        let target = self.playback_target(&route).await?;
        let selected = self.source.selected_library();
        self.queue.play(
            playback::PlayRequest::ordered(
                target.queue_input(
                    selected.as_ref().map(|selected| selected.source_key),
                    selected
                        .as_ref()
                        .and_then(|selected| selected.music_folder_key),
                ),
                0,
                queue_placement(&placement)?,
                true,
            )
            .with_context_title(context_title)
            .shuffled(shuffled),
        );
        Ok(())
    }

    pub async fn play_radio(
        &self,
        kind: String,
        media_uri: String,
        route: Option<String>,
        placement: String,
    ) -> Result<(), AndroidError> {
        let seed = if kind == "track" {
            library::RadioSeed::Track(media_uri)
        } else {
            let route =
                serde_json::from_str::<Route>(&route.ok_or_else(|| error("Choose a radio seed"))?)
                    .map_err(error)?;
            let database = self.database.clone();
            self.runtime
                .spawn(async move {
                    let cancel = ReadCancellation::new();
                    match route {
                        Route::AlbumDetail(uri) => database
                            .album_row_by_media_uri(&uri, &cancel)
                            .await
                            .map_err(error)?
                            .map(|row| library::RadioSeed::Album(row.album_key)),
                        Route::ArtistDetail(uri) => database
                            .artist_row_by_media_uri(&uri, &cancel)
                            .await
                            .map_err(error)?
                            .map(|row| library::RadioSeed::Artist(row.artist_key)),
                        Route::AlbumArtistDetail(uri) => database
                            .artist_row_by_media_uri(&uri, &cancel)
                            .await
                            .map_err(error)?
                            .map(|row| library::RadioSeed::AlbumArtist(row.artist_key)),
                        Route::GenreDetail(key) => Some(library::RadioSeed::Genre(key)),
                        Route::PlaylistDetail(key) => Some(library::RadioSeed::Playlist(key)),
                        _ => None,
                    }
                    .ok_or_else(|| error("This item has no radio action"))
                })
                .await
                .map_err(error)??
        };
        self.radio.play_radio(playback::RadioPlayRequest {
            seed,
            placement: queue_placement(&placement)?,
        });
        Ok(())
    }

    pub fn is_pinned(&self, pin: String) -> Result<bool, AndroidError> {
        let pin = serde_json::from_str::<SidebarPin>(&pin).map_err(error)?;
        Ok(self.settings.sidebar_changes().borrow().is_pinned(&pin))
    }

    pub async fn refresh_home(&self, section_id: String) -> Result<(), AndroidError> {
        let kind = serde_json::from_value(serde_json::Value::String(section_id)).map_err(error)?;
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        selected
            .operations
            .refresh_home(kind)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn configured_routes(&self) -> Vec<AndroidRouteDescriptor> {
        self.settings
            .sidebar_changes()
            .borrow()
            .route_items
            .iter()
            .filter(|item| item.visible)
            .map(|item| route_descriptor(item.item))
            .collect()
    }

    pub fn route_settings(&self) -> Vec<AndroidRouteSetting> {
        self.settings
            .sidebar_changes()
            .borrow()
            .route_items
            .iter()
            .map(|item| AndroidRouteSetting {
                descriptor: route_descriptor(item.item),
                visible: item.visible,
            })
            .collect()
    }

    pub async fn set_route_visible(&self, id: String, visible: bool) -> Result<bool, AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_route_visible(item, visible))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn move_route(&self, id: String, up: bool) -> Result<bool, AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.move_route(item, if up { -1 } else { 1 }))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn reorder_route(
        &self,
        id: String,
        target_id: String,
        after: bool,
    ) -> Result<(), AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let target =
            SidebarRouteItem::from_stable_id(&target_id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings.update_preferences(move |ui| {
                    ui.sidebar.reorder_route(item, target, after);
                    Ok(())
                })
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn play_rows(
        &self,
        context: String,
        media_uris: Vec<String>,
        index: u64,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let placement = queue_placement(&placement)?;
        self.queue.play(
            playback::PlayRequest::captured(
                library::QueueInput::Uris {
                    order: media_uris.into(),
                    context_id: context.into(),
                    source_start: 0,
                },
                usize::try_from(index).map_err(error)?,
                placement,
                false,
            )
            .with_context_title(context_title.map(|title| {
                Arc::new(library::QueueContextTitle {
                    kind: None,
                    title: title.into(),
                })
            }))
            .shuffled(shuffled),
        );
        Ok(())
    }

    pub fn collection_categories(&self) -> Vec<AndroidCollectionCategory> {
        CollectionCategory::ALL
            .into_iter()
            .map(|category| AndroidCollectionCategory {
                id: category.name().into(),
                title: localization::tr(category.title()),
                icon_name: category.icon_name().into(),
            })
            .collect()
    }

    pub fn random_settings(&self) -> Result<String, AndroidError> {
        serde_json::to_string(&self.settings.load().random_play).map_err(error)
    }

    pub async fn random_genre(
        &self,
        settings: String,
        route: Option<String>,
    ) -> Result<String, AndroidError> {
        let mut settings: rufin_core::settings::RandomPlaySettings =
            serde_json::from_str(&settings).map_err(error)?;
        let Some(route) = route else {
            settings.genre = None;
            return serde_json::to_string(&settings).map_err(error);
        };
        let Route::GenreDetail(key) = serde_json::from_str(&route).map_err(error)? else {
            return Err(error("Choose a genre"));
        };
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        selected
            .runtime
            .clone()
            .spawn(async move {
                let genre = selected
                    .database
                    .genre_rows(
                        selected.source_key,
                        &[key],
                        selected.music_folder_key,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)?
                    .pop()
                    .ok_or_else(|| error("Genre is no longer available"))?;
                settings.genre = Some(rufin_core::settings::RandomPlayGenreSelection {
                    source_id: selected.source_id,
                    music_folder_id: selected.music_folder_object_id,
                    genre_id: genre.object_id,
                });
                serde_json::to_string(&settings).map_err(error)
            })
            .await
            .map_err(error)?
    }

    pub async fn random_genre_title(&self, settings: String) -> Result<String, AndroidError> {
        let settings: rufin_core::settings::RandomPlaySettings =
            serde_json::from_str(&settings).map_err(error)?;
        let Some(selected) = self.source.selected_library() else {
            return Ok(localization::tr("Any genre"));
        };
        selected
            .runtime
            .clone()
            .spawn(async move {
                let Some(object_id) = settings.selected_genre_id(
                    &selected.source_id,
                    selected.music_folder_object_id.as_deref(),
                ) else {
                    return Ok(localization::tr("Any genre"));
                };
                let Some(key) = selected
                    .database
                    .genre_key_by_object(selected.source_key, object_id, &ReadCancellation::new())
                    .await
                    .map_err(error)?
                else {
                    return Ok(localization::tr("Any genre"));
                };
                Ok(selected
                    .database
                    .genre_rows(
                        selected.source_key,
                        &[key],
                        selected.music_folder_key,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)?
                    .pop()
                    .map_or_else(|| localization::tr("Any genre"), |genre| genre.name))
            })
            .await
            .map_err(error)?
    }

    pub async fn random_play(
        &self,
        settings: String,
        placement: String,
    ) -> Result<bool, AndroidError> {
        let settings: rufin_core::settings::RandomPlaySettings =
            serde_json::from_str(&settings).map_err(error)?;
        let placement = queue_placement(&placement)?;
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        let mut current = self.settings.load();
        current.random_play = settings.clone();
        self.settings.save(&current).map_err(error)?;
        let queue = self.queue.clone();
        selected
            .runtime
            .clone()
            .spawn(async move {
                let genre = if let Some(object) = settings.selected_genre_id(
                    &selected.source_id,
                    selected.music_folder_object_id.as_deref(),
                ) {
                    selected
                        .database
                        .genre_key_by_object(selected.source_key, object, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                } else {
                    None
                };
                rufin_core::radio::queue_random(
                    &selected.database,
                    selected.source_key,
                    selected.music_folder_key,
                    rufin_core::radio::random_play_request(&settings, genre, placement),
                    &queue,
                )
                .await
                .map_err(error)
            })
            .await
            .map_err(error)?
    }

    pub async fn add_to_playlist(
        &self,
        route: String,
        media_uri: String,
        skip_duplicates: bool,
    ) -> Result<u64, AndroidError> {
        let Route::PlaylistDetail(key) = serde_json::from_str(&route).map_err(error)? else {
            return Err(error("Choose a playlist"));
        };
        rufin_core::playlists::add_playlist_tracks(
            &self.source,
            key,
            vec![media_uri],
            skip_duplicates,
        )
        .recv()
        .await
        .map_err(error)?
        .map(|count| count as u64)
        .map_err(error)
    }

    pub fn routes(&self) -> Vec<AndroidRouteDescriptor> {
        SidebarRouteItem::all()
            .into_iter()
            .map(route_descriptor)
            .collect()
    }

    pub fn browse(
        &self,
        route: String,
        category: String,
        filter: String,
        sort: String,
        descending: bool,
        favorites_only: bool,
        downloaded_only: bool,
    ) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        self.browse_query(
            serde_json::from_str(&route).map_err(error)?,
            category,
            filter,
            sort,
            descending,
            favorites_only,
            downloaded_only,
            None,
        )
    }

    pub async fn search(&self, text: String) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        rufin_core::source::search::acquire_search(&selected, text, 60, ReadCancellation::new())
            .await
            .map(search_rows)
            .map_err(error)
    }

    pub async fn prepare_collection(&self, media_uri: String) -> Result<(), AndroidError> {
        self.source
            .prepare_collection(media_uri)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn favorite(
        &self,
        kind: String,
        media_uri: String,
        favorite: bool,
    ) -> Result<bool, AndroidError> {
        let target = favorite_target(&kind, media_uri)?;
        tracing::info!(
            kind = target.kind(),
            favorite,
            "Android library favorite requested"
        );
        self.source
            .set_favorite_with_result(target, favorite)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn set_rating(
        &self,
        kind: String,
        media_uri: String,
        value: Option<u8>,
    ) -> Result<(), AndroidError> {
        self.source
            .set_rating(favorite_target(&kind, media_uri)?, value);
        Ok(())
    }

    pub async fn rating(
        &self,
        kind: String,
        media_uri: String,
        source_id: Option<String>,
    ) -> Result<AndroidRatingState, AndroidError> {
        let target = favorite_target(&kind, media_uri)?;
        let half_stars = self
            .source
            .list_sources()
            .half_stars_enabled(target.media_uri(), source_id.as_deref());
        let visible = self.settings.load().context_menu.rating_visible;
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let cancellation = ReadCancellation::new();
                let user_rating = database
                    .user_media_state(target.media_uri(), &cancellation)
                    .await
                    .map_err(error)?
                    .and_then(|(_, rating)| rating);
                let value = if user_rating.is_some() {
                    user_rating
                } else {
                    let rating = match &target {
                        library::FavoriteTarget::Track(uri) => database
                            .track_row_by_uri(uri, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.rating),
                        library::FavoriteTarget::Album(uri) => database
                            .album_row_by_media_uri(uri, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.rating),
                        library::FavoriteTarget::Artist(uri) => database
                            .artist_row_by_media_uri(uri, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.rating),
                    };
                    rating.and_then(|value| u8::try_from(value).ok())
                };
                Ok(AndroidRatingState {
                    value,
                    half_stars,
                    visible,
                })
            })
            .await
            .map_err(error)?
    }

    pub async fn create_playlist(
        &self,
        source_id: Option<String>,
        name: String,
        media_uris: Vec<String>,
        public: Option<bool>,
    ) -> Result<Option<String>, AndroidError> {
        let source = source_id.map(sources::SourceId::new);
        let created = rufin_core::playlists::create_playlist(
            &self.source,
            source.clone(),
            name,
            media_uris,
            public,
        )
        .recv()
        .await
        .map_err(error)?
        .map_err(error)?;
        if let Some(id) = &created {
            self.settings
                .remember_created_playlist(source, id.clone(), None)
                .map_err(error)?;
        }
        Ok(created)
    }

    pub async fn import_playlist(
        &self,
        uri: String,
    ) -> Result<AndroidPlaylistImport, AndroidError> {
        let current = self
            .source
            .selected_library()
            .as_ref()
            .and_then(|selected| self.source.configuration(&selected.source_id));
        let imported = rufin_core::playlists::import_document_playlist(&self.source, uri, current)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let row = self
            .database
            .playlist_rows(&[imported.playlist], &ReadCancellation::new())
            .await
            .map_err(error)?
            .pop()
            .ok_or_else(|| error("Playlist no longer exists"))?;
        Ok(AndroidPlaylistImport {
            row: row.into(),
            skipped: imported.skipped,
        })
    }

    pub async fn rename_playlist(
        &self,
        route: String,
        name: String,
        public: Option<bool>,
    ) -> Result<bool, AndroidError> {
        let Route::PlaylistDetail(key) = serde_json::from_str(&route).map_err(error)? else {
            return Err(error("Choose a playlist"));
        };
        rufin_core::playlists::update_playlist(&self.source, key, Some(name), public, None)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn delete_playlist(&self, route: String) -> Result<bool, AndroidError> {
        let Route::PlaylistDetail(key) = serde_json::from_str(&route).map_err(error)? else {
            return Err(error("Choose a playlist"));
        };
        rufin_core::playlists::delete_playlist(&self.source, key)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn set_pin(&self, pin: String, pinned: bool) -> Result<bool, AndroidError> {
        self.settings
            .set_pinned(serde_json::from_str(&pin).map_err(error)?, pinned)
            .map_err(error)
    }

    pub fn reorder_pin(&self, moved: String, target: String) -> Result<bool, AndroidError> {
        self.settings
            .reorder_pin(
                &serde_json::from_str(&moved).map_err(error)?,
                &serde_json::from_str(&target).map_err(error)?,
            )
            .map_err(error)
    }

    pub fn pin_count(&self) -> u64 {
        self.settings.sidebar_changes().borrow().pins.len() as u64
    }

    pub async fn pins(&self, offset: u64, limit: u32) -> Result<AndroidPinPage, AndroidError> {
        let sidebar = self.settings.sidebar_changes();
        if !sidebar.borrow().pins_visible {
            return Ok(AndroidPinPage {
                rows: Vec::new(),
                next_offset: None,
                total: 0,
            });
        }
        let total = sidebar.borrow().pins.len() as u64;
        let pins = sidebar
            .borrow()
            .pins
            .iter()
            .skip(usize::try_from(offset).map_err(error)?)
            .take(limit.min(256) as usize)
            .cloned()
            .collect::<Vec<_>>();
        let consumed = offset.saturating_add(pins.len() as u64);
        let next_offset = (consumed < total).then_some(consumed);
        let db = self.database.clone();
        let selected = self.source.selected_library();
        self.runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let mut rows = Vec::new();
                for pin in pins {
                    let source = match &pin {
                        SidebarPin::Album { source_id, .. }
                        | SidebarPin::Artist { source_id, .. }
                        | SidebarPin::Genre { source_id, .. } => {
                            db.source_identity_key(source_id).await.map_err(error)?
                        }
                        _ => None,
                    };
                    let row = match &pin {
                        SidebarPin::Album { album_id, .. } => {
                            if let Some(source) = source {
                                if let Some(key) = db
                                    .album_key_by_object(source, album_id, &cancel)
                                    .await
                                    .map_err(error)?
                                {
                                    db.album_rows(source, &[key], None, &cancel)
                                        .await
                                        .map_err(error)?
                                        .pop()
                                        .map(Into::into)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        SidebarPin::Artist {
                            artist_id,
                            album_artist,
                            ..
                        } => {
                            if let Some(source) = source {
                                if let Some(key) = db
                                    .artist_key_by_object(source, artist_id, &cancel)
                                    .await
                                    .map_err(error)?
                                {
                                    db.artist_rows(source, &[key], *album_artist, None, &cancel)
                                        .await
                                        .map_err(error)?
                                        .pop()
                                        .map(|row| artist_row(row, *album_artist))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        SidebarPin::Genre { genre_id, .. } => {
                            if let Some(source) = source {
                                if let Some(key) = db
                                    .genre_key_by_object(source, genre_id, &cancel)
                                    .await
                                    .map_err(error)?
                                {
                                    db.genre_rows(source, &[key], None, &cancel)
                                        .await
                                        .map_err(error)?
                                        .pop()
                                        .map(Into::into)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        SidebarPin::Playlist {
                            source_id,
                            playlist_id,
                        } => {
                            if let Some(key) = db
                                .playlist_key_by_identity(source_id.as_ref(), playlist_id, &cancel)
                                .await
                                .map_err(error)?
                            {
                                db.playlist_rows(&[key], &cancel)
                                    .await
                                    .map_err(error)?
                                    .pop()
                                    .map(Into::into)
                            } else {
                                None
                            }
                        }
                        SidebarPin::SmartPlaylist { playlist_id } => {
                            if let Some(key) = db
                                .smart_playlist_key_by_object(playlist_id, &cancel)
                                .await
                                .map_err(error)?
                            {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_err(error)?
                                    .as_secs() as i64;
                                db.smart_playlist_rows(
                                    selected.as_ref().map(|selected| selected.source_key),
                                    &[key],
                                    selected
                                        .as_ref()
                                        .and_then(|selected| selected.music_folder_key),
                                    now,
                                    &cancel,
                                )
                                .await
                                .map_err(error)?
                                .pop()
                                .map(Into::into)
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(mut row) = row {
                        row.source_id = match &pin {
                            SidebarPin::Album { source_id, .. }
                            | SidebarPin::Artist { source_id, .. }
                            | SidebarPin::Genre { source_id, .. } => Some(source_id.to_string()),
                            SidebarPin::Playlist { source_id, .. } => {
                                source_id.as_ref().map(ToString::to_string)
                            }
                            SidebarPin::SmartPlaylist { .. } => None,
                        };
                        row.pin = Some(serde_json::to_string(&pin).map_err(error)?);
                        rows.push(row);
                    }
                }
                Ok(AndroidPinPage {
                    rows,
                    next_offset,
                    total,
                })
            })
            .await
            .map_err(error)?
    }
}
