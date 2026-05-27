fn build_library(
    scanned: Vec<ScannedTrack>,
    root_entries: Vec<LocalFolderEntry>,
    folders: HashMap<FolderId, LocalFolderEntry>,
) -> LocalLibrary {
    let mut albums = BTreeMap::<AlbumId, AlbumAccumulator>::new();
    let mut artists = BTreeMap::<ArtistId, ArtistAccumulator>::new();
    let mut album_artists = BTreeMap::<ArtistId, ArtistAccumulator>::new();
    let mut genres = BTreeMap::<GenreId, GenreAccumulator>::new();
    let mut covers = HashMap::new();
    let mut tracks = Vec::with_capacity(scanned.len());

    for mut scanned_track in scanned {
        let cover = scanned_track.cover.take();
        let track = &mut scanned_track.track;
        let album_entry =
            albums
                .entry(track.album_id.clone())
                .or_insert_with(|| AlbumAccumulator {
                    album: Album {
                        id: track.album_id.clone(),
                        title: track.album.clone(),
                        artist: scanned_track.album_artist.clone(),
                        artist_id: track
                            .album_artist_credits
                            .first()
                            .map(|artist| artist.id.clone()),
                        album_artist_credits: track.album_artist_credits.clone(),
                        artist_credits: track.artist_credits.clone(),
                        year: track.year,
                        release_date: track.release_date.clone(),
                        date_added: None,
                        last_played: None,
                        play_count: None,
                        user_rating: None,
                        track_count: 0,
                        duration_seconds: 0,
                        favorite: false,
                        color_seed: stable_hash(track.album_id.as_str()) as u32,
                        image_ref: None,
                        genres: Vec::new(),
                    },
                    album_artist_keys: BTreeSet::new(),
                    artist_keys: BTreeSet::new(),
                });
        if album_entry.album.image_ref.is_none()
            && let Some(cover) = cover
        {
            let cover_id = cover_id(&cover);
            covers.entry(cover_id.clone()).or_insert(cover);
            album_entry.album.image_ref = Some(ImageRef::new(cover_id, None));
        }
        album_entry.album.track_count = album_entry.album.track_count.saturating_add(1);
        album_entry.album.duration_seconds = album_entry
            .album
            .duration_seconds
            .saturating_add(track.duration_seconds);
        if album_entry.album.year == 0 {
            album_entry.album.year = track.year;
        }
        merge_genres(&mut album_entry.album.genres, &track.genres);

        for artist in &track.artist_credits {
            album_entry
                .artist_keys
                .insert(artist.id.as_str().to_string());
            artists
                .entry(artist.id.clone())
                .or_insert_with(|| ArtistAccumulator {
                    name: artist.name.clone(),
                    ..ArtistAccumulator::default()
                })
                .tracks
                .insert(track.id.clone());
            artists
                .entry(artist.id.clone())
                .or_insert_with(|| ArtistAccumulator {
                    name: artist.name.clone(),
                    ..ArtistAccumulator::default()
                })
                .albums
                .insert(track.album_id.clone());
        }
        for artist in &track.album_artist_credits {
            album_entry
                .album_artist_keys
                .insert(artist.id.as_str().to_string());
            album_artists
                .entry(artist.id.clone())
                .or_insert_with(|| ArtistAccumulator {
                    name: artist.name.clone(),
                    ..ArtistAccumulator::default()
                })
                .albums
                .insert(track.album_id.clone());
        }
        for genre_name in &track.genres {
            let genre_id = local_id("genre", genre_name);
            let genre = genres.entry(genre_id).or_insert_with(|| GenreAccumulator {
                name: genre_name.clone(),
                ..GenreAccumulator::default()
            });
            genre.albums.insert(track.album_id.clone());
            genre.tracks.insert(track.id.clone());
        }
        tracks.push(track.clone());
    }

    let album_image_refs = albums
        .iter()
        .filter_map(|(id, entry)| {
            entry
                .album
                .image_ref
                .clone()
                .map(|image_ref| (id.clone(), image_ref))
        })
        .collect::<HashMap<_, _>>();
    for track in &mut tracks {
        track.image_ref = album_image_refs.get(&track.album_id).cloned();
    }

    let mut albums = albums
        .into_values()
        .map(|entry| entry.album)
        .collect::<Vec<_>>();
    albums.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then(left.artist.to_lowercase().cmp(&right.artist.to_lowercase()))
    });

    let mut artists = artists
        .into_iter()
        .map(|(id, artist)| artist_from_accumulator(id, artist))
        .collect::<Vec<_>>();
    artists.sort_by_key(|artist| artist.name.to_lowercase());

    let mut album_artists = album_artists
        .into_iter()
        .map(|(id, artist)| artist_from_accumulator(id, artist))
        .collect::<Vec<_>>();
    album_artists.sort_by_key(|artist| artist.name.to_lowercase());

    let mut genres = genres
        .into_iter()
        .map(|(id, genre)| Genre {
            id,
            name: genre.name,
            album_count: genre.albums.len().min(u32::MAX as usize) as u32,
            track_count: genre.tracks.len().min(u32::MAX as usize) as u32,
            image_ref: None,
        })
        .collect::<Vec<_>>();
    genres.sort_by_key(|genre| genre.name.to_lowercase());

    LocalLibrary {
        roots: root_entries,
        folders,
        albums,
        tracks,
        artists,
        album_artists,
        genres,
        covers,
    }
}
fn artist_from_accumulator(id: ArtistId, artist: ArtistAccumulator) -> Artist {
    Artist {
        id,
        name: artist.name,
        album_count: artist.albums.len().min(u32::MAX as usize) as u32,
        track_count: artist.tracks.len().min(u32::MAX as usize) as u32,
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        image_ref: None,
    }
}
fn page<T: Clone>(items: &[T], request: PagedRequest) -> PagedResponse<T> {
    let start = request.offset.min(items.len());
    let end = start.saturating_add(request.limit).min(items.len());
    PagedResponse::new(items[start..end].to_vec(), items.len())
}
fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["mp3", "flac", "m4a", "wav", "ogg", "opus", "mp4", "mka"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}
fn folder_cover(dir: &Path) -> Option<PathBuf> {
    ["cover", "folder", "front", "album"]
        .into_iter()
        .flat_map(|stem| {
            ["jpg", "jpeg", "png", "webp"].map(move |ext| dir.join(format!("{stem}.{ext}")))
        })
        .find(|path| path.is_file())
}
fn embedded_cover(
    path: &Path,
    tagged_file: Option<&lofty::file::TaggedFile>,
    tag: Option<&Tag>,
) -> Option<LocalCover> {
    let picture = tag
        .and_then(|tag| select_best_picture(tag.pictures()))
        .or_else(|| tagged_file.and_then(|file| select_best_picture_from_tags(file.tags())))?;
    Some(LocalCover::Embedded {
        path: path.to_path_buf(),
        content_type: picture.mime_type().map(ToString::to_string),
    })
}
fn select_best_picture(pictures: &[Picture]) -> Option<&Picture> {
    pictures
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
}
fn select_best_picture_from_tags(tags: &[Tag]) -> Option<&Picture> {
    tags.iter()
        .find_map(|tag| select_best_picture(tag.pictures()))
}
fn cover_id(cover: &LocalCover) -> String {
    let raw = match cover {
        LocalCover::File(path) => format!("file:{}", path.to_string_lossy()),
        LocalCover::Embedded { path, .. } => format!("embedded:{}", path.to_string_lossy()),
    };
    format!(
        "local:cover:{}",
        utf8_percent_encode(&raw, NON_ALPHANUMERIC)
    )
}
fn cover_url(cover: &LocalCover) -> ProviderResult<String> {
    match cover {
        LocalCover::File(path) | LocalCover::Embedded { path, .. } => Url::from_file_path(path)
            .map(|url| url.to_string())
            .map_err(|()| {
                ProviderError::Other("could not turn cover path into a file URI".to_string())
            }),
    }
}
fn content_type_from_path(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg".to_string()),
        Some("png") => Some("image/png".to_string()),
        Some("webp") => Some("image/webp".to_string()),
        _ => None,
    }
}
fn tag_string(tag: Option<&Tag>, read: impl FnOnce(&Tag) -> Option<String>) -> Option<String> {
    tag.and_then(read).filter(|value| !value.trim().is_empty())
}
fn artist_names(tag: Option<&Tag>, fallback: &str) -> Vec<String> {
    let tagged = tag
        .map(|tag| {
            tag.get_strings(ItemKey::TrackArtists)
                .flat_map(split_credit_names)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if tagged.is_empty() {
        split_credit_names(fallback)
    } else {
        tagged
    }
}
fn split_credit_names(value: &str) -> Vec<String> {
    let names = value
        .split([';', '/'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if names.is_empty() { Vec::new() } else { names }
}
fn album_grouping_path(path: &Path) -> String {
    path.parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn merge_genres(target: &mut Vec<String>, source: &[String]) {
    for genre in source {
        if !target.iter().any(|candidate| candidate == genre) {
            target.push(genre.clone());
        }
    }
}
fn local_id<T>(kind: &str, value: &str) -> T
where
    T: From<String>,
{
    T::from(format!("local:{kind}:{:016x}", stable_hash(value)))
}
fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
fn normalize_search(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn searchable_matches<'a>(query: &str, mut values: impl Iterator<Item = &'a String>) -> bool {
    values.any(|value| normalize_search(value).contains(query))
}
#[allow(dead_code)]
fn decode_cover_id(item_id: &str) -> Option<String> {
    item_id
        .strip_prefix("local:cover:")
        .and_then(|encoded| percent_decode_str(encoded).decode_utf8().ok())
        .map(|decoded| decoded.into_owned())
}
