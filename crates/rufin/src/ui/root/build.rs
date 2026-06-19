use super::*;

pub(in crate::ui) fn decoded_cover_candidate_sizes(preferred_size: u32) -> Vec<u32> {
    let mut sizes = Vec::from([preferred_size]);
    if preferred_size <= THUMB_COVER_SIZE {
        sizes.extend([THUMB_COVER_SIZE, GRID_COVER_SIZE, DETAIL_COVER_SIZE]);
    } else if preferred_size <= GRID_COVER_SIZE {
        sizes.extend([GRID_COVER_SIZE, DETAIL_COVER_SIZE]);
    } else {
        sizes.push(DETAIL_COVER_SIZE);
    }
    let mut seen = HashSet::new();
    sizes.retain(|size| seen.insert(*size));
    sizes
}
pub(in crate::ui) fn playback_artwork_cache_keys(
    server_id: &ServerId,
    image_ref: &ImageRef,
    preferred_size: u32,
) -> Vec<String> {
    decoded_cover_candidate_sizes(preferred_size)
        .into_iter()
        .map(|size| {
            image_cache_key(
                server_id,
                &image_ref.item_id,
                image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
                size,
            )
        })
        .collect()
}
pub(in crate::ui) fn playback_artwork_path_from_lookup(
    server_id: &ServerId,
    image_ref: &ImageRef,
    preferred_size: u32,
    mut lookup: impl FnMut(&str) -> Option<PathBuf>,
) -> Option<PlaybackArtworkPath> {
    playback_artwork_cache_keys(server_id, image_ref, preferred_size)
        .into_iter()
        .find_map(|key| lookup(&key).map(|path| PlaybackArtworkPath { key, path }))
}
pub(in crate::ui) fn playback_artwork_key_matches(
    server_id: &ServerId,
    image_ref: &ImageRef,
    preferred_size: u32,
    key: &str,
) -> bool {
    playback_artwork_cache_keys(server_id, image_ref, preferred_size)
        .iter()
        .any(|candidate| candidate == key)
}
pub(in crate::ui) fn notification_icon_path(path: &Path) -> Option<Vec<u8>> {
    let pixbuf = Pixbuf::from_file(path).ok()?;
    notification_icon_pixbuf(&pixbuf)
}
pub(in crate::ui) fn notification_icon_pixbuf(pixbuf: &Pixbuf) -> Option<Vec<u8>> {
    let target_size = THUMB_COVER_SIZE.clamp(1, 512) as i32;
    let width = pixbuf.width().max(1);
    let height = pixbuf.height().max(1);
    let crop_size = width.min(height);
    let crop_x = (width - crop_size) / 2;
    let crop_y = (height - crop_size) / 2;
    let cropped = Pixbuf::new(Colorspace::Rgb, pixbuf.has_alpha(), 8, crop_size, crop_size)?;
    pixbuf.copy_area(crop_x, crop_y, crop_size, crop_size, &cropped, 0, 0);
    let icon = if crop_size == target_size {
        cropped
    } else {
        cropped.scale_simple(target_size, target_size, InterpType::Bilinear)?
    };

    icon.save_to_bufferv("png", &[]).ok()
}
pub(in crate::ui) fn cover_decode_size(display_size: i32, fetch_size: u32) -> i32 {
    display_size.max(fetch_size as i32).max(1)
}
pub(in crate::ui) fn cover_fetch_size_for_display(display_size: i32) -> u32 {
    if display_size <= THUMB_COVER_SIZE as i32 {
        THUMB_COVER_SIZE
    } else if display_size <= GRID_COVER_SIZE as i32 {
        GRID_COVER_SIZE
    } else {
        DETAIL_COVER_SIZE
    }
}
pub(in crate::ui) fn first_run_cover_prime_refs(library: &LibrarySnapshot) -> Vec<ImageRef> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();

    for section in library
        .home_sections
        .iter()
        .take(FIRST_RUN_HOME_SECTION_LIMIT)
    {
        for album in section.albums.iter().take(HOME_COVER_LIMIT) {
            push_first_run_cover_ref(&mut refs, &mut seen, album.image_ref.as_ref());
        }
        for track in section.tracks.iter().take(HOME_COVER_LIMIT) {
            push_first_run_cover_ref(&mut refs, &mut seen, track.image_ref.as_ref());
        }
    }

    for track in library.tracks.iter().take(TRACK_ROUTE_PAGE_SIZE) {
        push_first_run_cover_ref(&mut refs, &mut seen, track.image_ref.as_ref());
    }
    for album in library.albums.iter().take(GRID_ROUTE_PAGE_SIZE) {
        push_first_run_cover_ref(&mut refs, &mut seen, album.image_ref.as_ref());
    }
    for artist in library.artists.iter().take(GRID_ROUTE_PAGE_SIZE) {
        push_first_run_cover_ref(&mut refs, &mut seen, artist.image_ref.as_ref());
    }
    for artist in library.album_artists.iter().take(GRID_ROUTE_PAGE_SIZE) {
        push_first_run_cover_ref(&mut refs, &mut seen, artist.image_ref.as_ref());
    }
    for genre in library.genres.iter().take(GRID_ROUTE_PAGE_SIZE) {
        for image_ref in &genre.image_refs {
            push_first_run_cover_ref(&mut refs, &mut seen, Some(image_ref));
        }
        push_first_run_cover_ref(&mut refs, &mut seen, genre.image_ref.as_ref());
    }
    for playlist in library.playlists.iter().take(GRID_ROUTE_PAGE_SIZE) {
        for image_ref in &playlist.image_refs {
            push_first_run_cover_ref(&mut refs, &mut seen, Some(image_ref));
        }
    }

    refs
}
pub(in crate::ui) fn push_first_run_cover_ref(
    refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    image_ref: Option<&ImageRef>,
) {
    if refs.len() >= GRID_COVER_LIMIT {
        return;
    }
    let Some(image_ref) = image_ref else {
        return;
    };
    let key = (
        image_ref.item_id.clone(),
        image_ref.tag.clone().unwrap_or_default(),
    );
    if seen.insert(key) {
        refs.push(image_ref.clone());
    }
}
pub(in crate::ui) fn prefetched_explore_from_snapshot(
    snapshot: &LibrarySnapshot,
) -> Option<PrefetchedHomeSection> {
    Some(PrefetchedHomeSection {
        server_id: snapshot.server.as_ref()?.id.clone(),
        section: snapshot.prefetched_explore.clone()?,
    })
}
pub(in crate::ui) fn upsert_snapshot_home_section(
    sections: &mut Vec<HomeSection>,
    section: HomeSection,
) {
    if let Some(existing) = sections
        .iter_mut()
        .find(|existing| existing.kind == section.kind)
    {
        *existing = section;
    } else if section.kind == HomeSectionKind::Explore {
        sections.insert(0, section);
    } else {
        sections.push(section);
    }
}
pub(in crate::ui) fn reset_home_section_pages(
    states: &mut HashMap<HomeSectionKind, HomeSectionState>,
) {
    states.clear();
}
