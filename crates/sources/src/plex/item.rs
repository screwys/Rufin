//! Read Plex metadata as normalized Library records.
use serde_json::Value;

use super::plex_id;
use crate::SourceResult;
use crate::remote_json::{field, id, items};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ItemResponse {
    Complete,
    Summary,
}

pub(super) fn read_items<'a>(
    source_id: &str,
    rows: impl IntoIterator<Item = (&'a Value, ItemResponse)>,
) -> SourceResult<Vec<library::CatalogBatch>> {
    let rows = rows.into_iter().collect::<Vec<_>>();
    let mut batches = Vec::new();
    for response in [ItemResponse::Complete, ItemResponse::Summary] {
        let mut tracks = Vec::new();
        let mut albums = Vec::new();
        let mut artists = Vec::new();
        let mut folders = Vec::new();
        let mut genre_rows = Vec::new();
        let mut playlists = Vec::new();
        for (item, _) in rows.iter().filter(|(_, kind)| *kind == response) {
            let Some(raw) = id(&item["ratingKey"]) else {
                continue;
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
                    crate::native_artwork_binding(
                        &source_id,
                        &crate::NativeImageRef::new(path, None),
                    )
                })
                .transpose()?;
            let rating = (matches!(response, ItemResponse::Complete)
                || item.get("userRating").is_some())
            .then(|| {
                field::<f64>(item, "userRating")
                    .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
                    .map(|v| (v.round() as i64) * 10)
            });
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
            let mut item_genres = genres(item);
            let mut folder = None;
            match kind {
                "artist" => {
                    let input = library::ArtistObservation {
                        metadata: library::ArtistMetadata {
                            name: title.to_owned(),
                            sort_text: Some(sort),
                            musicbrainz_artist_id: mbid.map(str::to_owned),
                            ..Default::default()
                        },
                        user_data: library::NativeUserState {
                            favorite: Some(false),
                            rating: rating,
                            ..Default::default()
                        },
                        object_id: object,
                        artwork_binding: artwork,
                    };
                    artists.push(input);
                }
                "album" => {
                    let artist = item["parentTitle"].as_str().unwrap_or("Unknown Artist");
                    let artists = id(&item["parentRatingKey"])
                        .map(|raw| library::ArtistCredit {
                            object_id: plex_id("artist", &raw),
                            name: artist.to_owned(),
                            ..Default::default()
                        })
                        .into_iter()
                        .collect();
                    let input = library::AlbumObservation {
                        metadata: library::AlbumMetadata {
                            title: title.to_owned(),
                            artist: artist.to_owned(),
                            sort_text: sort,
                            year: year,
                            release_date: release,
                            date_added: added_date,
                            musicbrainz_release_id: mbid.map(str::to_owned),
                            is_compilation: field::<i64>(item, "compilation").map(|v| v != 0),
                            album_artists: artists,
                            genres: std::mem::take(&mut item_genres),
                            ..Default::default()
                        },
                        user_data: library::NativeUserState {
                            favorite: Some(false),
                            rating: rating,
                            ..Default::default()
                        },
                        object_id: object,
                        artwork_binding: artwork,
                        first_seen_at: added,
                        ..Default::default()
                    };
                    albums.push(input);
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
                    let audio_properties = library::AudioProperties {
                        bitrate_kbps: stream
                            .and_then(|stream| field(stream, "bitrate"))
                            .or_else(|| field(media, "bitrate")),
                        sample_rate_hz: stream.and_then(|stream| field(stream, "samplingRate")),
                        bit_depth: stream.and_then(|stream| field(stream, "bitDepth")),
                        channels: stream
                            .and_then(|stream| field(stream, "channels"))
                            .or_else(|| field(media, "audioChannels")),
                    };
                    let duration = field::<i64>(item, "duration").unwrap_or_default().max(0);
                    let source_path = part["file"].as_str();
                    let format = media["container"].as_str().or(media["audioCodec"].as_str());
                    let comment = item["summary"].as_str();

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
                    let number = |key| {
                        stream
                            .and_then(|stream| field::<f64>(stream, key))
                            .filter(|v| v.is_finite())
                    };
                    let source_loudness = (number("loudness").is_some()
                        || number("gain").is_some())
                    .then(|| library::NativeGain {
                        integrated_lufs: number("loudness"),
                        replay_gain_db: number("gain"),
                        replay_gain_peak: number("peak").filter(|value| *value >= 0.0),
                        ..Default::default()
                    });
                    let album_loudness = number("albumGain").map(|gain| library::NativeGain {
                        replay_gain_db: Some(gain),
                        replay_gain_peak: number("albumPeak").filter(|value| *value >= 0.0),
                        ..Default::default()
                    });
                    let artists = id(&item["grandparentRatingKey"])
                        .map(|raw| library::ArtistCredit {
                            object_id: plex_id("artist", &raw),
                            name: item["grandparentTitle"]
                                .as_str()
                                .unwrap_or(artist)
                                .to_owned(),
                            ..Default::default()
                        })
                        .into_iter()
                        .collect();
                    let folders = id(&item["librarySectionID"]).map(|section| {
                        let object_id = plex_id("music-folder", &section);
                        folder = Some(library::FolderObservation {
                            metadata: library::FolderMetadata {
                                name: item["librarySectionTitle"].as_str().map(str::to_owned),
                                ..Default::default()
                            },
                            object_id: object_id.clone(),
                            ..Default::default()
                        });
                        vec![object_id]
                    });
                    let input = library::TrackObservation {
                        metadata: library::TrackMetadata {
                            title: title.to_owned(),
                            album: album.to_owned(),
                            artist: artist.to_owned(),
                            sort_text: sort,
                            duration_millis: duration,
                            disc_number: field::<i64>(item, "parentIndex").unwrap_or(1).max(0),
                            track_number: field::<i64>(item, "index").unwrap_or_default().max(0),
                            year: year,
                            release_date: release,
                            date_added: added_date,
                            source_format: format.map(str::to_owned),
                            comment: comment.map(str::to_owned),
                            bpm: field(item, "bpm"),
                            musicbrainz_recording_id: mbid.map(str::to_owned),
                            audio_properties: audio_properties,
                            source_loudness: source_loudness,
                            album_loudness: album_loudness,
                            artists: artists,
                            genres: std::mem::take(&mut item_genres),
                            moods: moods(item),
                            ..Default::default()
                        },
                        user_data: library::NativeUserState {
                            favorite: Some(false),
                            rating: rating,
                            play_count: field::<i64>(item, "viewCount").filter(|v| *v >= 0),
                            skip_count: field::<i64>(item, "skipCount").filter(|v| *v >= 0),
                            last_played: field::<i64>(item, "lastViewedAt").filter(|v| *v >= 0),
                            ..Default::default()
                        },
                        object_id: object,
                        album_object_id: album_id,
                        artwork_binding: artwork,
                        first_seen_at: added,
                        source_path: source_path.map(str::to_owned),
                        loudness_analysis_key: *hash.finalize().as_bytes(),
                        folders,
                        ..Default::default()
                    };
                    tracks.push(input);
                }
                "playlist" => {
                    let smart = crate::remote_json::boolean(&item["smart"])
                        .or_else(|| field::<i64>(item, "smart").map(|v| v != 0))
                        .unwrap_or(false);
                    playlists.push(library::PlaylistObservation {
                        object_id: object,
                        name: title.to_owned(),
                        sort_text: sort,
                        artwork_binding: artwork,
                        writable: Some(!smart),
                        metadata_writable: Some(!smart),
                        ..Default::default()
                    });
                }
                _ => {
                    continue;
                }
            }
            if let Some(folder) = folder {
                folders.push(folder);
            }
            genre_rows.extend(item_genres);
        }
        let batch = library::CatalogBatch {
            reference: response == ItemResponse::Summary,
            tracks: tracks,
            albums,
            artists,
            folders,
            playlists,
            genres: genre_rows
                .into_iter()
                .map(|tag| library::GenreObservation {
                    object_id: tag.object_id,
                    metadata: library::TagMetadata {
                        name: tag.name,
                        sort_text: tag.sort_text,
                    },
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        if !batch.tracks.is_empty()
            || !batch.albums.is_empty()
            || !batch.artists.is_empty()
            || !batch.folders.is_empty()
            || !batch.playlists.is_empty()
        {
            batches.push(batch);
        }
    }
    Ok(batches)
}

fn genres(item: &Value) -> Vec<library::TagReference> {
    items(&item["Genre"])
        .iter()
        .filter_map(|genre| genre["tag"].as_str().filter(|name| !name.is_empty()))
        .map(|name| {
            let sort_text = name.to_lowercase();
            library::TagReference {
                object_id: plex_id("genre", &sort_text),
                name: name.to_owned(),
                sort_text: Some(sort_text),
                ..Default::default()
            }
        })
        .collect()
}

fn moods(item: &Value) -> Vec<library::TagReference> {
    items(&item["Mood"])
        .iter()
        .filter_map(|mood| mood["tag"].as_str().filter(|name| !name.is_empty()))
        .map(|name| library::TagReference {
            object_id: plex_id("mood", &name.to_lowercase()),
            name: name.to_owned(),
            sort_text: Some(name.to_lowercase()),
        })
        .collect()
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
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = sender.send(update);
            std::future::ready(())
        });
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
        database
            .reconcile_source(&library::SourceId::new("plex"), "plex", "Plex")
            .await
            .unwrap();
        let catalog_source_id = ("plex").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();

        (async {
            for batch in read_items(
                &catalog_source_id,
                artists
                    .iter()
                    .chain(&albums)
                    .chain(&tracks)
                    .map(|item| (item, ItemResponse::Complete)),
            )? {
                catalog.apply_catalog(&batch, true).await?;
            }
            Ok::<_, crate::SourceError>(())
        })
        .await
        .unwrap();

        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        let publication = updates.try_recv().expect("catalog update");
        assert!(publication.metadata);
        for refresh in [false, true] {
            if refresh {
                let catalog_source_id = ("plex").to_string();
                let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();

                (async {
                    for batch in read_items(
                        &catalog_source_id,
                        albums
                            .iter()
                            .chain(&tracks)
                            .map(|item| (item, ItemResponse::Complete)),
                    )? {
                        catalog.apply_catalog(&batch, true).await?;
                    }
                    Ok::<_, crate::SourceError>(())
                })
                .await
                .unwrap();

                {
                    database
                        .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                        .await;
                    Ok::<_, crate::SourceError>(())
                }
                .unwrap();
            }
            let rows = library::ListRead {
                playlist_folder: None,
                kind: library::ListKind::Artists {
                    album_artist: false,
                },
                source: Some(
                    database
                        .source_identity_key(&library::SourceId::new(&catalog_source_id))
                        .await
                        .unwrap()
                        .unwrap(),
                ),
                sort: library::CatalogSort::Title,
                folder: None,
                filter: ("").to_string(),
                descending: false,
                favorites_only: false,
                downloaded_only: false,
                activity: false,
            }
            .page_artists(
                &database,
                library::RouteSeedWindow::top(),
                &library::ReadCancellation::new(),
            )
            .await
            .map(|page| page.rows)
            .unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| row.metadata.name.as_str())
                    .collect::<Vec<_>>(),
                ["The Cure", "Duran Duran"]
            );
        }
    }
}
