use super::*;

fn route_descriptor(item: SidebarRouteItem) -> AndroidRouteDescriptor {
    let descriptor = sidebar_route_descriptor(item);
    AndroidRouteDescriptor {
        item,
        id: descriptor.stable_id.into(),
        title: localization::tr(descriptor.title),
        icon_name: descriptor.icon_name.into(),
        selected_icon_name: descriptor.selected_icon_name().into(),
        route: descriptor.root_route,
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn target_download_status(
        &self,
        route: Option<Route>,
        media_uri: String,
        source_id: Option<String>,
    ) -> Result<records::AndroidTargetDownloadStatus, AndroidError> {
        let _ = source_id;
        let query = route
            .as_ref()
            .map(|route| self.target_query(route.clone()))
            .transpose()?;
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let (total, downloaded) = if let Some(query) = query {
                    let selection = query.selection().await?.as_ref().clone();
                    if let MemberSelection::Members { mut read, .. } = selection {
                        let total = read.count(&database, &cancel).await.map_err(error)? as i64;
                        read.downloaded_only = true;
                        (
                            total,
                            read.count(&database, &cancel).await.map_err(error)? as i64,
                        )
                    } else {
                        let uris = database
                            .selected_track_uris(&selection, None, &cancel)
                            .await
                            .map_err(error)?;
                        (
                            uris.len() as i64,
                            database
                                .downloaded_media_count(&uris, &cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                } else {
                    (
                        1,
                        database
                            .downloaded_media_count(&[media_uri], &cancel)
                            .await
                            .map_err(error)?,
                    )
                };
                Ok(records::AndroidTargetDownloadStatus {
                    total: total.max(0) as u64,
                    downloaded: downloaded.max(0) as u64,
                })
            })
            .await
            .map_err(error)?
    }

    pub async fn download_target(
        &self,
        route: Option<Route>,
        media_uri: String,
        remove: bool,
    ) -> Result<(), AndroidError> {
        let query = route
            .as_ref()
            .map(|route| self.target_query(route.clone()))
            .transpose()?;
        let source = self.source.clone();
        self.runtime
            .spawn(async move {
                let selection = match query {
                    Some(query) => query.selection().await?.as_ref().clone(),
                    None => MemberSelection::Uris(vec![media_uri].into()),
                };
                let result = if remove {
                    source.remove_download_selection(selection)
                } else {
                    source.download_selection(
                        selection,
                        route
                            .map(|route| {
                                serde_json::to_string(&route).expect("Route serialization")
                            })
                            .unwrap_or_else(|| "track".into()),
                        None,
                    )
                };
                result.recv().await.map_err(error)?.map_err(error)
            })
            .await
            .map_err(error)?
    }
}

impl AndroidLibrary {
    pub(super) fn browse_query(
        &self,
        route: Route,
        category: String,
        filter: String,
        sort: Option<LibraryField>,
        descending: bool,
        favorites_only: bool,
        downloaded_only: bool,
    ) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        let history_current_only = category == "current";
        let category = CollectionCategory::from_name(&category);
        let settings = self.settings.load();
        let display = settings.library_list(library_list_key(&route, category));
        let (sort, descending) = sort.map_or((display.sort_key, display.descending), |sort| {
            (sort, descending)
        });
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
            playback: self.playback.clone(),
            route,
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
            release_section: None,
            opened: tokio::sync::OnceCell::new(),
            filter,
            sort,
            descending,
            blocks: settings.application.home_blocks,
        }))
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
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        self.source
            .set_music_folder(selected.source_id, id)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn set_browse_display(
        &self,
        route: Route,
        category: String,
        layout: LibraryLayout,
        size: DisplaySize,
        grid_spacing: DisplaySize,
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        let key = library_list_key(&route, CollectionCategory::from_name(&category));
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
        route: Route,
        category: String,
        sort: LibraryField,
        descending: bool,
    ) -> Result<(), AndroidError> {
        let key = library_list_key(&route, CollectionCategory::from_name(&category));
        self.settings
            .update_library_list_settings(key, |settings| {
                settings.sort_key = sort;
                settings.descending = descending;
            })
            .map_err(error)?;
        Ok(())
    }

    pub async fn play_target(
        &self,
        route: Route,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let query = self.target_query(route)?;
        let context_title = context_caption(&query.route, context_title);
        let placement = queue_placement(&placement)?;
        let selection = query.clone().selection().await?;
        self.playback.play_selection(
            rufin_core::playback::PlayRequest::ordered(
                selection.as_ref().clone(),
                0,
                placement,
                true,
            )
            .with_context(query.context_id().into())
            .with_context_title(context_title)
            .shuffled(shuffled),
        );
        Ok(())
    }

    pub async fn play_radio(
        &self,
        kind: String,
        media_uri: String,
        route: Option<Route>,
        placement: String,
    ) -> Result<(), AndroidError> {
        let seed = if kind == "track" {
            library::RadioSeed::Track(media_uri)
        } else {
            let route = route.ok_or_else(|| error("Choose a radio seed"))?;
            let database = self.database.clone();
            self.runtime
                .spawn(async move {
                    let cancel = ReadCancellation::new();
                    match route {
                        Route::AlbumDetail(uri) => database
                            .album(&uri, false, &cancel)
                            .await
                            .map_err(error)?
                            .map(|row| library::RadioSeed::Album(row.album_key)),
                        Route::ArtistDetail(uri) => database
                            .artist(&uri, false, false, &cancel)
                            .await
                            .map_err(error)?
                            .map(|row| library::RadioSeed::Artist(row.artist_key)),
                        Route::AlbumArtistDetail(uri) => database
                            .artist(&uri, true, false, &cancel)
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
        self.playback
            .play_radio(rufin_core::playback::RadioPlayRequest {
                seed,
                placement: queue_placement(&placement)?,
            });
        Ok(())
    }

    pub fn is_pinned(&self, pin: SidebarPin) -> Result<bool, AndroidError> {
        Ok(self.settings.sidebar_changes().borrow().is_pinned(&pin))
    }

    pub async fn refresh_home(&self, section_id: String) -> Result<(), AndroidError> {
        let kind = serde_json::from_value(serde_json::Value::String(section_id)).map_err(error)?;
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        self.source
            .refresh_home(selected.source_id, kind)
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
        self.playback.play_selection(
            rufin_core::playback::PlayRequest::uris(media_uris, placement)
                .with_context(context.into())
                .with_anchor_index(usize::try_from(index).map_err(error)?)
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

    pub async fn random_genre(
        &self,
        route: Option<Route>,
    ) -> Result<Option<rufin_core::settings::RandomPlayGenreSelection>, AndroidError> {
        let Some(route) = route else {
            return Ok(None);
        };
        let Route::GenreDetail(key) = route else {
            return Err(error("Choose a genre"));
        };
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let genre = database
                    .genres(&[key], selected.music_folder_key, &ReadCancellation::new())
                    .await
                    .map_err(error)?
                    .pop()
                    .ok_or_else(|| error("Genre is no longer available"))?;
                Ok(Some(rufin_core::settings::RandomPlayGenreSelection {
                    source_id: selected.source_id,
                    music_folder_id: selected.music_folder_object_id,
                    genre_id: genre.object_id,
                }))
            })
            .await
            .map_err(error)?
    }

    pub async fn random_genre_title(
        &self,
        settings: rufin_core::settings::RandomPlaySettings,
    ) -> Result<String, AndroidError> {
        let Some(selected) = self.source.selected_library() else {
            return Ok(localization::tr("Any genre"));
        };
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let Some(object_id) = settings.selected_genre_id(
                    &selected.source_id,
                    selected.music_folder_object_id.as_deref(),
                ) else {
                    return Ok(localization::tr("Any genre"));
                };
                let Some(key) = database
                    .catalog_keys(
                        library::CatalogKind::Genre,
                        Some(selected.source_key),
                        &[(object_id).to_string()],
                        &ReadCancellation::new(),
                    )
                    .await
                    .map(|keys| keys.into_iter().next().map(library::GenreKey::from_raw))
                    .map_err(error)?
                else {
                    return Ok(localization::tr("Any genre"));
                };
                Ok(database
                    .genres(&[key], selected.music_folder_key, &ReadCancellation::new())
                    .await
                    .map_err(error)?
                    .pop()
                    .map_or_else(
                        || localization::tr("Any genre"),
                        |genre| genre.metadata.name,
                    ))
            })
            .await
            .map_err(error)?
    }

    pub async fn random_play(
        &self,
        settings: rufin_core::settings::RandomPlaySettings,
        placement: String,
    ) -> Result<bool, AndroidError> {
        let placement = queue_placement(&placement)?;
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        let owner = self.settings.clone();
        let settings = self
            .runtime
            .spawn_blocking(move || owner.set_random_preferences(settings))
            .await
            .map_err(error)?
            .map_err(error)?;
        let queue = self.playback.clone();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let genre = if let Some(object) = settings.selected_genre_id(
                    &selected.source_id,
                    selected.music_folder_object_id.as_deref(),
                ) {
                    database
                        .catalog_keys(
                            library::CatalogKind::Genre,
                            Some(selected.source_key),
                            &[(object).to_string()],
                            &ReadCancellation::new(),
                        )
                        .await
                        .map(|keys| keys.into_iter().next().map(library::GenreKey::from_raw))
                        .map_err(error)?
                } else {
                    None
                };
                rufin_core::radio::queue_random(
                    &database,
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

    pub fn routes(&self) -> Vec<AndroidRouteDescriptor> {
        SidebarRouteItem::all()
            .into_iter()
            .map(route_descriptor)
            .collect()
    }

    pub fn browse(
        &self,
        route: Route,
        category: String,
        filter: String,
        sort: Option<LibraryField>,
        descending: bool,
        favorites_only: bool,
        downloaded_only: bool,
    ) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        self.browse_query(
            route,
            category,
            filter,
            sort,
            descending,
            favorites_only,
            downloaded_only,
        )
    }

    pub async fn search(&self, text: String) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        let source = self.source.clone();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                rufin_core::source::search::acquire_search(
                    &source,
                    &database,
                    &selected,
                    text,
                    60,
                    ReadCancellation::new(),
                )
                .await
            })
            .await
            .map_err(error)?
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

    pub async fn rating(
        &self,
        target: library::FavoriteTarget,
    ) -> Result<Option<u8>, AndroidError> {
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
                            .track(uri, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.state.rating),
                        library::FavoriteTarget::Album(uri) => database
                            .album(uri, false, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.state.rating),
                        library::FavoriteTarget::Artist(uri) => database
                            .artist(uri, false, false, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.state.rating),
                    };
                    rating.and_then(|value| u8::try_from(value).ok())
                };
                Ok(value)
            })
            .await
            .map_err(error)?
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
            .playlists(&[imported.playlist], &ReadCancellation::new())
            .await
            .map_err(error)?
            .pop()
            .ok_or_else(|| error("Playlist no longer exists"))?;
        Ok(AndroidPlaylistImport {
            row: row.into(),
            skipped: imported.skipped,
        })
    }

    pub fn set_pin(&self, pin: SidebarPin, pinned: bool) -> Result<bool, AndroidError> {
        self.settings.set_pinned(pin, pinned).map_err(error)
    }

    pub fn reorder_pin(&self, moved: SidebarPin, target: SidebarPin) -> Result<bool, AndroidError> {
        self.settings.reorder_pin(&moved, &target).map_err(error)
    }

    pub fn pin_count(&self) -> u64 {
        self.settings.sidebar_changes().borrow().pins.len() as u64
    }

    pub fn pins_use_activity(&self) -> bool {
        self.settings.load().sidebar.pins_use_activity()
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
                    let row = match &pin {
                        SidebarPin::Album {
                            source_id,
                            album_id,
                        } => {
                            let uri = library::source_entity_uri(source_id, "album", album_id);
                            db.album(&uri, false, &cancel)
                                .await
                                .map_err(error)?
                                .map(Into::into)
                        }
                        SidebarPin::Artist {
                            source_id,
                            artist_id,
                            album_artist,
                        } => {
                            let uri = library::source_entity_uri(source_id, "artist", artist_id);
                            db.artist(&uri, *album_artist, false, &cancel)
                                .await
                                .map_err(error)?
                                .map(|row| artist_row(row, *album_artist))
                        }
                        SidebarPin::Genre {
                            source_id,
                            genre_id,
                        } => {
                            let uri = library::source_entity_uri(source_id, "genre", genre_id);
                            db.genre(&uri, None, &cancel)
                                .await
                                .map_err(error)?
                                .map(Into::into)
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
                                db.playlists(&[key], &cancel)
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
                        row.context_source_id = match &pin {
                            SidebarPin::Album { source_id, .. }
                            | SidebarPin::Artist { source_id, .. }
                            | SidebarPin::Genre { source_id, .. } => Some(source_id.to_string()),
                            SidebarPin::Playlist { source_id, .. } => {
                                source_id.as_ref().map(ToString::to_string)
                            }
                            SidebarPin::SmartPlaylist { .. } => None,
                        };
                        row.pin = Some(pin);
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

#[uniffi::export]
impl AndroidLibrary {
    pub fn target_query(&self, route: Route) -> Result<Arc<AndroidBrowseQuery>, AndroidError> {
        self.browse_query(
            match route {
                Route::ArtistDetail(uri) | Route::ArtistDiscography(uri) => {
                    Route::ArtistTracks(uri)
                }
                Route::AlbumArtistDetail(uri) | Route::AlbumArtistDiscography(uri) => {
                    Route::AlbumArtistTracks(uri)
                }
                route => route,
            },
            "tracks".into(),
            String::new(),
            None,
            false,
            false,
            false,
        )
    }
}
