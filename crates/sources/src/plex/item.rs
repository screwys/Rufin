//! Map independent Plex metadata facts directly into the Store scan.
use library::Scan;
use serde_json::Value;

use super::plex_id;
use crate::SourceResult;
use crate::remote_json::{field, id, items};

pub(super) async fn stage_item(scan: &mut Scan, item: &Value) -> SourceResult<()> {
    let Some(raw) = id(&item["ratingKey"]) else {
        return Ok(());
    };
    let kind = item["type"].as_str().unwrap_or_default();
    let object = plex_id(kind, &raw);
    let title = item["title"].as_str().unwrap_or("Untitled");
    let sort = item["titleSort"]
        .as_str()
        .filter(|sort| !sort.trim().is_empty())
        .unwrap_or(title)
        .to_lowercase();
    let artwork = ["thumb", "parentThumb", "grandparentThumb"]
        .iter()
        .find_map(|key| item[*key].as_str())
        .map(|path| {
            crate::native_artwork_binding(scan.source_id(), &crate::NativeImageRef::new(path, None))
        })
        .transpose()?;
    let rating = field::<f64>(item, "userRating")
        .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
        .map(|v| v.round() as i64);
    let year = field::<i64>(item, "year").filter(|v| *v > 0).or_else(|| {
        if kind == "track" {
            field::<i64>(item, "parentYear").filter(|v| *v > 0)
        } else {
            None
        }
    });
    let release = crate::policy::normalized_date(field(item, "originallyAvailableAt"));
    let added = field::<i64>(item, "addedAt").filter(|v| *v >= 0);
    let added_date = added.map(epoch_date);
    let mbid = items(&item["Guid"])
        .iter()
        .filter_map(|v| v["id"].as_str())
        .find_map(|v| v.strip_prefix("mbid://"));
    match kind {
        "artist" => {
            scan.write_artist(
                &object,
                title,
                &title.to_lowercase(),
                Some(&sort),
                mbid,
                artwork.as_deref(),
                Some(false),
                rating,
            )
            .await?
        }
        "album" => {
            let artist = item["parentTitle"].as_str().unwrap_or("Unknown Artist");
            scan.write_album(
                &object,
                title,
                &title.to_lowercase(),
                artist,
                &sort,
                year,
                release.as_deref(),
                added_date.as_deref(),
                mbid,
                None,
                field::<i64>(item, "compilation").map(|v| v != 0),
                artwork.as_deref(),
                false,
                rating,
                added,
            )
            .await?;
            if let Some(raw_artist) = id(&item["parentRatingKey"]) {
                let artist_id = plex_id("artist", &raw_artist);
                scan.write_artist(
                    &artist_id,
                    artist,
                    &artist.to_lowercase(),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
                scan.write_album_relations(&[(&object, &artist_id)], &[], &[])
                    .await?;
            }
        }
        "track" => {
            let artist = item["originalTitle"]
                .as_str()
                .or(item["grandparentTitle"].as_str())
                .unwrap_or("Unknown Artist");
            let album = item["parentTitle"].as_str().unwrap_or("Unknown Album");
            let album_id = id(&item["parentRatingKey"]).map(|raw| plex_id("album", &raw));
            let media = &item["Media"][0];
            let part = &media["Part"][0];
            let stream = items(&part["Stream"])
                .iter()
                .find(|stream| field::<i64>(stream, "streamType") == Some(2));
            let duration = field::<i64>(item, "duration").unwrap_or_default().max(0);
            let source_path = part["file"].as_str();
            let format = media["container"].as_str().or(media["audioCodec"].as_str());
            let comment = item["summary"].as_str();
            let search =
                format!("{title} {album} {artist} {}", comment.unwrap_or_default()).to_lowercase();
            let mut hash = blake3::Hasher::new();
            for fact in [
                object.as_str(),
                source_path.unwrap_or_default(),
                format.unwrap_or_default(),
            ] {
                hash.update(&(fact.len() as u64).to_le_bytes());
                hash.update(fact.as_bytes());
            }
            hash.update(&duration.to_le_bytes());
            scan.write_track(
                &object,
                album_id.as_deref(),
                title,
                &search,
                album,
                artist,
                &sort,
                duration,
                field::<i64>(item, "parentIndex").unwrap_or(1).max(0),
                field::<i64>(item, "index").unwrap_or_default().max(0),
                year,
                release.as_deref(),
                added_date.as_deref(),
                None,
                format,
                comment,
                field(item, "bpm"),
                mbid,
                None,
                None,
                None,
                None,
                artwork.as_deref(),
                false,
                rating,
                added,
                field::<i64>(item, "viewCount").filter(|v| *v >= 0),
                field::<i64>(item, "skipCount").filter(|v| *v >= 0),
                field::<i64>(item, "lastViewedAt").filter(|v| *v >= 0),
                source_path,
                *hash.finalize().as_bytes(),
            )
            .await?;
            if let Some(stream) = stream {
                let number = |key| field::<f64>(stream, key).filter(|v| v.is_finite());
                if number("loudness").is_some() || number("gain").is_some() {
                    scan.write_track_source_loudness(
                        &object,
                        number("loudness"),
                        None,
                        number("gain"),
                        number("peak").filter(|value| *value >= 0.0),
                    )
                    .await?;
                }
                if let Some(album_id) = &album_id
                    && number("albumGain").is_some()
                {
                    scan.write_album_source_loudness(
                        album_id,
                        None,
                        None,
                        number("albumGain"),
                        number("albumPeak").filter(|value| *value >= 0.0),
                    )
                    .await?;
                }
            }
            if let Some(raw_artist) = id(&item["grandparentRatingKey"]) {
                let artist_id = plex_id("artist", &raw_artist);
                let album_artist = item["grandparentTitle"].as_str().unwrap_or(artist);
                scan.write_artist(
                    &artist_id,
                    album_artist,
                    &album_artist.to_lowercase(),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
                scan.write_track_relations(&[(&object, &artist_id)], &[], &[])
                    .await?;
            }
            if let Some(section) = id(&item["librarySectionID"]) {
                let folder_id = plex_id("music-folder", &section);
                if let Some(name) = item["librarySectionTitle"].as_str() {
                    scan.write_folder(
                        &folder_id,
                        name,
                        &name.to_lowercase(),
                        &name.to_lowercase(),
                        None,
                    )
                    .await?;
                } else {
                    scan.write_folder_reference(&folder_id).await?;
                }
                scan.write_track_folders(&[library::ScanLink::new(&object, &folder_id, 0)])
                    .await?;
            }
        }
        "playlist" => {
            scan.write_playlist(
                &object,
                title,
                &title.to_lowercase(),
                &sort,
                artwork.as_deref(),
            )
            .await?;
            let smart = crate::remote_json::boolean(&item["smart"])
                .or_else(|| field::<i64>(item, "smart").map(|v| v != 0))
                .unwrap_or(false);
            scan.write_playlist_writable(&object, !smart).await?;
        }
        _ => return Ok(()),
    }
    for genre in items(&item["Genre"]) {
        let Some(name) = genre["tag"].as_str().filter(|v| !v.is_empty()) else {
            continue;
        };
        let genre_id = plex_id("genre", &name.to_lowercase());
        scan.write_genre(
            &genre_id,
            name,
            &name.to_lowercase(),
            &name.to_lowercase(),
            None,
        )
        .await?;
        match kind {
            "track" => {
                scan.write_track_relations(&[], &[(&object, &genre_id)], &[])
                    .await?
            }
            "album" => {
                scan.write_album_relations(&[], &[(&object, &genre_id)], &[])
                    .await?
            }
            _ => (),
        }
    }
    if kind == "track" {
        for mood in items(&item["Mood"]) {
            let Some(name) = mood["tag"].as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            let mood_id = plex_id("mood", &name.to_lowercase());
            scan.write_mood(&mood_id, name, &name.to_lowercase(), &name.to_lowercase())
                .await?;
            scan.write_track_relations(&[], &[], &[(&object, &mood_id)])
                .await?;
        }
    }
    Ok(())
}

fn epoch_date(seconds: i64) -> String {
    let days = seconds / 86_400 + 719_468;
    let era = days / 146_097;
    let day = days - era * 146_097;
    let year = (day - day / 1460 + day / 36524 - day / 146096) / 365;
    let doy = day - (365 * year + year / 4 - year / 100);
    let month = (5 * doy + 2) / 153;
    let date = doy - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{date:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn artist_title_sort_survives_full_import_and_album_track_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let artists = [
            json!({"ratingKey":"608","type":"artist","title":"The Cure","titleSort":"Cure, The"}),
            json!({"ratingKey":"609","type":"artist","title":"Duran Duran"}),
        ];
        let albums = [
            json!({"ratingKey":"album-608","type":"album","title":"Wish","parentRatingKey":"608","parentTitle":"The Cure"}),
            json!({"ratingKey":"album-609","type":"album","title":"Rio","parentRatingKey":"609","parentTitle":"Duran Duran"}),
        ];
        let tracks = [
            json!({"ratingKey":"track-608","type":"track","title":"Friday I'm in Love","parentRatingKey":"album-608","parentTitle":"Wish","grandparentRatingKey":"608","grandparentTitle":"The Cure"}),
            json!({"ratingKey":"track-609","type":"track","title":"Rio","parentRatingKey":"album-609","parentTitle":"Rio","grandparentRatingKey":"609","grandparentTitle":"Duran Duran"}),
        ];
        let mut scan = Scan::begin(&database, "plex", "Plex", "plex", None)
            .await
            .unwrap();
        for item in artists.iter().chain(&albums).chain(&tracks) {
            stage_item(&mut scan, item).await.unwrap();
        }
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("initial import")
        };
        for refresh in [false, true] {
            if refresh {
                let mut scan = Scan::begin_items(&database, "plex").await.unwrap();
                for item in albums.iter().chain(&tracks) {
                    stage_item(&mut scan, item).await.unwrap();
                }
                scan.finish().await.unwrap();
            }
            let (_, _, rows) = database
                .artist_route_page(
                    publication.source,
                    None,
                    false,
                    false,
                    "",
                    library::ArtistSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
                ["The Cure", "Duran Duran"]
            );
        }
    }
}
