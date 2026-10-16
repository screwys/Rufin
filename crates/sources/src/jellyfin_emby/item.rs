use std::path::Path;

pub(super) use crate::NativeImageRef as ImageRef;
use crate::remote_json::{boolean, field, id, items, strings};
use serde_json::Value;

use crate::policy::{normalized_date, u16_from_option};

use super::ServerKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Playlist {
    pub id: String,
    pub name: String,
    pub image_ref: Option<ImageRef>,
    pub duration_seconds: u32,
    pub track_count: usize,
}

pub(super) const ALBUM_FIELDS: &str = "SortName,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,ImageBlurHashes,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount";
pub(super) const TRACK_FIELDS: &str = "SortName,Path,Overview,Container,MediaStreams,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,ImageBlurHashes,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,NormalizationGain,AlbumNormalizationGain";
pub(super) const PLAYLIST_FIELDS: &str = "RunTimeTicks,ImageTags,ImageBlurHashes,ChildCount";
pub(super) const MIXED_ITEM_FIELDS: &str = "SortName,Path,Overview,Container,MediaStreams,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,ParentId,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,ImageBlurHashes,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount,AlbumCount,SongCount,NormalizationGain,AlbumNormalizationGain";

pub(super) fn album_from_item(
    server: ServerKind,
    source: &str,
    item: &Value,
) -> library::LibraryResult<Option<library::AlbumObservation>> {
    let Some(item_id) = id(&item["Id"]) else {
        return Ok(None);
    };
    let image_ref = primary_image_ref(server, "album", &item_id, item)
        .or_else(|| alternate_primary_image_ref(server, item))
        .or_else(|| backdrop_image_ref(server, item));
    let mut album_artists = artist_credits_from_pairs(server, &item["AlbumArtists"]);
    let display_artist = album_display_artist(item, &album_artists);
    if !item["AlbumArtists"].is_array() {
        album_artists = artist_credits_from_pairs(server, &item["ArtistItems"]);
    }
    let title = field(item, "Name").unwrap_or_else(|| "Untitled Album".to_string());
    Ok(Some(library::AlbumObservation {
        metadata: library::AlbumMetadata {
            sort_text: sort_name(item)
                .unwrap_or_else(|| title.clone())
                .to_lowercase(),
            title: title,
            artist: display_artist,
            year: Some(i64::from(u16_from_option(field(item, "ProductionYear"))))
                .filter(|year| *year > 0),
            release_date: normalized_date(field(item, "PremiereDate")),
            date_added: normalized_date(field(item, "DateCreated")),
            musicbrainz_release_id: source_id(&item["ProviderIds"], "MusicBrainzAlbum"),
            musicbrainz_release_group_id: source_id(
                &item["ProviderIds"],
                "MusicBrainzReleaseGroup",
            ),
            album_artists: album_artists,
            genres: genre_credits_from_pairs(server, &item["GenreItems"]),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            favorite: item["UserData"]
                .is_object()
                .then(|| favorite(&item["UserData"])),
            rating: item["UserData"]
                .is_object()
                .then(|| user_rating(&item["UserData"]).map(|rating| i64::from(rating) * 10)),
            ..Default::default()
        },
        object_id: server.object_id("album", &item_id),
        artwork_binding: image_ref
            .as_ref()
            .map(|image| crate::native_artwork_binding(source, image))
            .transpose()?,
        ..Default::default()
    }))
}

pub(super) fn track_from_item(
    server: ServerKind,
    source: &str,
    item: &Value,
) -> library::LibraryResult<Option<library::TrackObservation>> {
    let Some(item_id) = id(&item["Id"]) else {
        return Ok(None);
    };
    let image_ref = album_image_ref(server, item)
        .or_else(|| primary_image_ref(server, "track", &item_id, item))
        .or_else(|| alternate_primary_image_ref(server, item))
        .or_else(|| backdrop_image_ref(server, item));
    let artists = artist_credits_from_pairs(server, &item["ArtistItems"]);
    let album_object_id = id(&item["AlbumId"])
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| server.object_id("album", id));
    let source_path = field::<String>(item, "Path");
    let source_format = source_format_from_item(
        field::<String>(item, "Container").as_deref(),
        source_path.as_deref(),
    );
    let stream = items(&item["MediaStreams"])
        .iter()
        .find(|stream| stream["Type"].as_str() == Some("Audio"));
    let title = field(item, "Name").unwrap_or_else(|| "Untitled Track".to_string());
    let object_id = server.object_id("track", &item_id);
    let duration_seconds = duration_seconds(field(item, "RunTimeTicks"));
    let mut audio = blake3::Hasher::new();
    audio.update(b"rufin-jellyfin-audio-v1\0");
    for value in [
        object_id.as_str(),
        source_path.as_deref().unwrap_or_default(),
        source_format.as_deref().unwrap_or_default(),
    ] {
        audio.update(&(value.len() as u64).to_le_bytes());
        audio.update(value.as_bytes());
    }
    audio.update(&duration_seconds.to_le_bytes());
    Ok(Some(library::TrackObservation {
        metadata: library::TrackMetadata {
            sort_text: sort_name(item)
                .unwrap_or_else(|| title.clone())
                .to_lowercase(),
            title: title,
            artist: track_display_artist(item, &artists),
            album: field(item, "Album").unwrap_or_else(|| "Unknown Album".to_string()),
            year: Some(i64::from(u16_from_option(field(item, "ProductionYear"))))
                .filter(|year| *year > 0),
            release_date: normalized_date(field(item, "PremiereDate")),
            date_added: normalized_date(field(item, "DateCreated")),
            duration_millis: i64::from(duration_seconds) * 1_000,
            disc_number: i64::from(u16_from_option(field(item, "ParentIndexNumber"))),
            track_number: i64::from(u16_from_option(field(item, "IndexNumber"))),
            musicbrainz_recording_id: source_id(&item["ProviderIds"], "MusicBrainzRecording"),
            musicbrainz_release_track_id: source_id(&item["ProviderIds"], "MusicBrainzTrack"),
            source_format: source_format,
            comment: field::<String>(item, "Overview").filter(|value| !value.trim().is_empty()),
            source_loudness: field::<f64>(item, "NormalizationGain")
                .filter(|value| value.is_finite())
                .map(|replay_gain_db| library::NativeGain {
                    replay_gain_db: Some(replay_gain_db),
                    ..Default::default()
                }),
            album_loudness: field::<f64>(item, "AlbumNormalizationGain")
                .filter(|value| value.is_finite())
                .map(|replay_gain_db| library::NativeGain {
                    replay_gain_db: Some(replay_gain_db),
                    ..Default::default()
                }),
            audio_properties: library::AudioProperties {
                bitrate_kbps: stream
                    .and_then(|stream| field::<u32>(stream, "BitRate"))
                    .map(|value| value / 1000),
                sample_rate_hz: stream.and_then(|stream| field(stream, "SampleRate")),
                bit_depth: stream.and_then(|stream| field(stream, "BitDepth")),
                channels: stream.and_then(|stream| field(stream, "Channels")),
            },
            artists: artists,
            album_artists: artist_credits_from_pairs(server, &item["AlbumArtists"]),
            genres: genre_credits_from_pairs(server, &item["GenreItems"]),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            last_played: crate::policy::unix_seconds(field(&item["UserData"], "LastPlayedDate")),
            play_count: play_count(&item["UserData"]).map(i64::from),
            rating: item["UserData"]
                .is_object()
                .then(|| user_rating(&item["UserData"]).map(|rating| i64::from(rating) * 10)),
            favorite: item["UserData"]
                .is_object()
                .then(|| favorite(&item["UserData"])),
            ..Default::default()
        },
        object_id,
        album_object_id,
        artwork_binding: image_ref
            .as_ref()
            .map(|image| crate::native_artwork_binding(source, image))
            .transpose()?,
        source_path,
        loudness_analysis_key: *audio.finalize().as_bytes(),
        ..Default::default()
    }))
}

pub(super) fn home_entry_from_item(
    server: ServerKind,
    item: &Value,
    position: usize,
    kind: library::HomeEntryKind,
) -> Option<library::HomeEntryInput> {
    let item_id = id(&item["Id"])?;
    let (entity, title, subtitle) = match kind {
        library::HomeEntryKind::Track => (
            "track",
            field(item, "Name").unwrap_or_else(|| "Untitled Track".to_string()),
            track_display_artist(
                item,
                &artist_credits_from_pairs(server, &item["ArtistItems"]),
            ),
        ),
        library::HomeEntryKind::Album => (
            "album",
            field(item, "Name").unwrap_or_else(|| "Untitled Album".to_string()),
            album_display_artist(
                item,
                &artist_credits_from_pairs(server, &item["AlbumArtists"]),
            ),
        ),
        _ => unreachable!(),
    };
    Some(library::HomeEntryInput {
        position: position as i64,
        kind,
        entity_object_id: server.object_id(entity, &item_id),
        title,
        subtitle,
    })
}

fn album_display_artist(item: &Value, credits: &[library::ArtistCredit]) -> String {
    field::<String>(item, "AlbumArtist")
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| joined_credit_names(credits))
        .or_else(|| joined_artist_names(Some(&strings(&item["Artists"]))))
        .unwrap_or_else(|| "Unknown Artist".to_string())
}

fn track_display_artist(item: &Value, credits: &[library::ArtistCredit]) -> String {
    joined_artist_names(Some(&strings(&item["Artists"])))
        .or_else(|| joined_credit_names(credits))
        .unwrap_or_else(|| {
            field(item, "AlbumArtist").unwrap_or_else(|| "Unknown Artist".to_string())
        })
}

fn source_format_from_item(container: Option<&str>, path: Option<&str>) -> Option<String> {
    let extension = path
        .map(|path| {
            if path.contains(['?', '#'])
                && url::Url::parse(path)
                    .is_ok_and(|uri| uri.scheme().len() > 1 && !uri.cannot_be_a_base())
            {
                path.split(['?', '#']).next().unwrap_or(path)
            } else {
                path
            }
        })
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .and_then(|path| Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let container = container.map(str::trim).filter(|value| !value.is_empty());
    if let Some(container) = container {
        if container.contains(',')
            && let Some(extension) = extension
            && container
                .split(',')
                .any(|alias| alias.trim().eq_ignore_ascii_case(extension))
        {
            return Some(extension.to_owned());
        }
        return container
            .split(',')
            .next()
            .map(|alias| alias.trim().to_owned());
    }
    extension.map(ToString::to_string)
}

pub(super) fn is_audio_item(item: &Value) -> bool {
    item["Type"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("Audio"))
}

pub(super) fn artist_from_item(
    server: ServerKind,
    source: &str,
    item: &Value,
) -> library::LibraryResult<Option<library::ArtistObservation>> {
    let Some(item_id) = id(&item["Id"]) else {
        return Ok(None);
    };
    let name = field(item, "Name").unwrap_or_else(|| "Unknown Artist".to_string());
    Ok(Some(library::ArtistObservation {
        metadata: library::ArtistMetadata {
            sort_text: Some(
                sort_name(item)
                    .unwrap_or_else(|| name.clone())
                    .to_lowercase(),
            ),
            name: name,
            musicbrainz_artist_id: source_id(&item["ProviderIds"], "MusicBrainzArtist"),
            ..Default::default()
        },
        user_data: library::NativeUserState {
            favorite: item["UserData"]
                .is_object()
                .then(|| favorite(&item["UserData"])),
            rating: item["UserData"]
                .is_object()
                .then(|| user_rating(&item["UserData"]).map(|rating| i64::from(rating) * 10)),
            ..Default::default()
        },
        object_id: server.object_id("artist", &item_id),
        artwork_binding: primary_image_ref(server, "artist", &item_id, item)
            .as_ref()
            .map(|image| crate::native_artwork_binding(source, image))
            .transpose()?,
    }))
}

pub(super) fn genre_from_item(
    server: ServerKind,
    source: &str,
    item: &Value,
) -> library::LibraryResult<Option<library::GenreObservation>> {
    let Some(item_id) = id(&item["Id"]) else {
        return Ok(None);
    };
    let name = field(item, "Name").unwrap_or_else(|| "Unknown Genre".to_string());
    Ok(Some(library::GenreObservation {
        object_id: server.object_id("genre", &item_id),
        metadata: library::TagMetadata {
            sort_text: Some(name.to_lowercase()),
            name,
        },
        artwork_binding: primary_image_ref(server, "genre", &item_id, item)
            .as_ref()
            .map(|image| crate::native_artwork_binding(source, image))
            .transpose()?,
    }))
}

pub(super) fn playlist_from_item(server: ServerKind, item: &Value) -> Option<Playlist> {
    let item_id = id(&item["Id"])?;
    Some(Playlist {
        id: String::from(server.object_id("playlist", &item_id)),
        name: field(item, "Name").unwrap_or_else(|| "Untitled Playlist".to_string()),
        image_ref: primary_image_ref(server, "playlist", &item_id, item),
        duration_seconds: duration_seconds(field(item, "RunTimeTicks")),
        track_count: field::<i32>(item, "ChildCount")
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
    })
}

fn artist_credits_from_pairs(server: ServerKind, pairs: &Value) -> Vec<library::ArtistCredit> {
    items(pairs)
        .iter()
        .filter_map(|pair| {
            Some(library::ArtistCredit {
                object_id: server.object_id("artist", &id(&pair["Id"])?),
                name: field::<String>(pair, "Name")
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Unknown Artist".into()),
                ..Default::default()
            })
        })
        .collect()
}
fn genre_credits_from_pairs(server: ServerKind, pairs: &Value) -> Vec<library::TagReference> {
    items(pairs)
        .iter()
        .filter_map(|pair| {
            Some(library::TagReference {
                object_id: server.object_id("genre", &id(&pair["Id"])?),
                name: field::<String>(pair, "Name")
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Unknown Genre".into()),
                ..Default::default()
            })
        })
        .collect()
}

fn joined_credit_names(credits: &[library::ArtistCredit]) -> Option<String> {
    let names = credits
        .iter()
        .map(|credit| credit.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn joined_artist_names(artists: Option<&[String]>) -> Option<String> {
    let names = artists
        .unwrap_or_default()
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn source_id(ids: &Value, key: &str) -> Option<String> {
    field::<String>(ids, key).filter(|value| !value.trim().is_empty())
}

fn duration_seconds(ticks: Option<i64>) -> u32 {
    ticks
        .map(|value| (value.max(0) / 10_000_000) as u32)
        .unwrap_or(0)
}

pub(super) fn favorite(data: &Value) -> bool {
    boolean(&data["IsFavorite"]).unwrap_or(false)
}
pub(super) fn play_count(data: &Value) -> Option<u32> {
    field::<i32>(data, "PlayCount").map(|v| v.max(0) as u32)
}
pub(super) fn user_rating(data: &Value) -> Option<u8> {
    field::<f64>(data, "Rating")
        .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
        .map(|v| v.round() as u8)
}

pub(super) fn primary_image_ref(
    server: ServerKind,
    kind: &str,
    item_id: &str,
    item: &Value,
) -> Option<ImageRef> {
    let tag = field::<String>(&item["ImageTags"], "Primary").filter(|tag| !tag.is_empty())?;
    Some(ImageRef {
        item_id: server.object_id(kind, item_id),
        blur_hash: image_blur_hash(item, "Primary", &tag),
        tag: Some(tag),
    })
}

fn image_blur_hash(item: &Value, kind: &str, tag: &str) -> Option<String> {
    field::<String>(&item["ImageBlurHashes"][kind], tag).filter(|hash| !hash.is_empty())
}
fn alternate_primary_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let tag = field::<String>(item, "PrimaryImageTag").filter(|tag| !tag.trim().is_empty())?;
    let owner = id(&item["PrimaryImageItemId"]).or_else(|| id(&item["Id"]))?;
    Some(ImageRef {
        item_id: server.object_id("track", &owner),
        blur_hash: image_blur_hash(item, "Primary", &tag),
        tag: Some(tag),
    })
}

fn sort_name(item: &Value) -> Option<String> {
    field::<String>(item, "SortName")
        .filter(|name| !name.trim().is_empty())
        .or_else(|| field::<String>(item, "ForcedSortName").filter(|name| !name.trim().is_empty()))
}
fn album_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let album_id = id(&item["AlbumId"])?;
    let tag = field::<String>(item, "AlbumPrimaryImageTag").filter(|tag| !tag.trim().is_empty())?;
    Some(ImageRef {
        item_id: server.object_id("album", &album_id),
        blur_hash: image_blur_hash(item, "Primary", tag.trim()),
        tag: Some(tag.trim().to_string()),
    })
}
fn backdrop_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let (item_id, tags) = if first_image_tag(&item["BackdropImageTags"]).is_some() {
        (id(&item["Id"])?, &item["BackdropImageTags"])
    } else {
        (
            id(&item["ParentBackdropItemId"])?,
            &item["ParentBackdropImageTags"],
        )
    };
    let tag = first_image_tag(tags);
    Some(ImageRef {
        item_id: server.object_id("backdrop", &item_id),
        blur_hash: tag
            .as_deref()
            .and_then(|tag| image_blur_hash(item, "Backdrop", tag)),
        tag,
    })
}
fn first_image_tag(tags: &Value) -> Option<String> {
    strings(tags)
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .find(|tag| !tag.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn emby_import_preserves_distinct_alternate_album_covers() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        database
            .reconcile_source(&library::SourceId::new("emby"), "emby", "Emby")
            .await
            .unwrap();
        let catalog_source_id = ("emby").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        catalog.start_observation().await.unwrap();
        let albums: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/alternate-album-covers.json")).unwrap();
        for album in &albums {
            let mut mapped = album_from_item(ServerKind::Emby, catalog_source_id.as_str(), &album)
                .unwrap()
                .unwrap();
            mapped.artwork_binding = Some(
                crate::native_artwork_binding(
                    catalog_source_id.as_str(),
                    &ImageRef::new(
                        "emby:backdrop:3259",
                        Some("c9c414f53f4439cf148b7d4bca58e300".into()),
                    ),
                )
                .unwrap(),
            );
            (async {
                let mut batch = library::CatalogBatch::default();
                let result = batch.albums.extend_from_slice(&[(mapped).clone()]);
                catalog.apply_catalog(&batch, true).await?;
                Ok::<_, library::LibraryError>(result)
            })
            .await
            .unwrap();
        }
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        let catalog_source_id = ("emby").to_string();
        let mut catalog = database.catalog(&catalog_source_id, None).await.unwrap();
        for album in &albums {
            (async {
                let mut batch = library::CatalogBatch::default();
                let result = batch.albums.extend_from_slice(&[(album_from_item(
                    ServerKind::Emby,
                    catalog_source_id.as_str(),
                    &album,
                )
                .unwrap()
                .unwrap())
                .clone()]);
                catalog.apply_catalog(&batch, true).await?;
                Ok::<_, library::LibraryError>(result)
            })
            .await
            .unwrap();
        }
        {
            database
                .publish_catalog_update(&catalog_source_id, catalog.changes().clone())
                .await;
            Ok::<_, crate::SourceError>(())
        }
        .unwrap();
        for album in albums {
            let object = format!("emby:album:{}", album["Id"].as_str().unwrap());
            let uri = library::source_entity_uri(&crate::SourceId::new("emby"), "album", &object);
            let row =
                library::CatalogQuery::uris(library::CatalogKind::Album, vec![(&uri).to_owned()])
                    .rows(&database, 0, 256, &library::ReadCancellation::new())
                    .await
                    .map(|rows| match rows {
                        library::CatalogRows::Albums(rows) => rows.into_iter().next(),
                        _ => unreachable!(),
                    })
                    .unwrap()
                    .unwrap();
            assert_eq!(row.metadata.title, album["Name"].as_str().unwrap());
            let binding: Vec<library::ArtworkCandidate> =
                serde_json::from_slice(row.artwork_binding.as_deref().unwrap()).unwrap();
            assert_eq!(
                match &binding[0].image {
                    library::ArtworkImage::Native { item_id, .. } => item_id.as_str(),
                    _ => panic!("native image"),
                },
                format!(
                    "emby:track:{}",
                    album["PrimaryImageItemId"].as_str().unwrap()
                )
            );
            assert_eq!(
                match &binding[0].image {
                    library::ArtworkImage::Native { tag, .. } => tag.as_deref(),
                    _ => panic!("native image"),
                },
                album["PrimaryImageTag"].as_str()
            );
        }
    }

    #[test]
    fn effective_sort_names_and_existing_image_precedence_are_preserved() {
        for server in [ServerKind::Jellyfin, ServerKind::Emby] {
            let item = json!({"Id":"1", "Name":"The Name", "SortName":"effective override", "ForcedSortName":"Raw override"});
            assert_eq!(
                album_from_item(server, "source", &item)
                    .unwrap()
                    .unwrap()
                    .metadata
                    .sort_text,
                "effective override"
            );
            assert_eq!(
                artist_from_item(server, "source", &item)
                    .unwrap()
                    .unwrap()
                    .metadata
                    .sort_text
                    .as_deref(),
                Some("effective override")
            );
            assert_eq!(
                track_from_item(server, "source", &item)
                    .unwrap()
                    .unwrap()
                    .metadata
                    .sort_text,
                "effective override"
            );
            let forced = album_from_item(
                server,
                "source",
                &json!({"Id":"1", "Name":"Name", "ForcedSortName":"Override"}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(forced.metadata.sort_text, "override");
            let plain = album_from_item(
                server,
                "source",
                &json!({"Id":"1", "Name":"Name", "SortName":"", "ForcedSortName":null}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(plain.metadata.sort_text, "name");
            assert_eq!(plain.metadata.title, "Name");
            let item = json!({"Id":"1", "ImageTags":{"Primary":"own"}, "PrimaryImageItemId":"2", "PrimaryImageTag":"alternate", "AlbumId":"3", "AlbumPrimaryImageTag":"album", "ImageBlurHashes":{"Primary":{"own":"own-preview","album":"album-preview","alternate":"alternate-preview"}}});
            let album = album_from_item(server, "source", &item).unwrap().unwrap();
            let album_binding: Vec<library::ArtworkCandidate> =
                serde_json::from_slice(album.artwork_binding.as_deref().unwrap()).unwrap();
            assert_eq!(
                match &album_binding[0].image {
                    library::ArtworkImage::Native { item_id, .. } => item_id.as_str(),
                    _ => panic!("native image"),
                },
                server.object_id("album", "1")
            );
            assert_eq!(album_binding[0].blur_hash.as_deref(), Some("own-preview"));
            let track = track_from_item(server, "source", &item).unwrap().unwrap();
            let track_binding: Vec<library::ArtworkCandidate> =
                serde_json::from_slice(track.artwork_binding.as_deref().unwrap()).unwrap();
            assert_eq!(
                match &track_binding[0].image {
                    library::ArtworkImage::Native { item_id, .. } => item_id.as_str(),
                    _ => panic!("native image"),
                },
                server.object_id("album", "3")
            );
            assert_eq!(track_binding[0].blur_hash.as_deref(), Some("album-preview"));
            let alternate = alternate_primary_image_ref(server, &item).unwrap();
            assert_eq!(alternate.blur_hash.as_deref(), Some("alternate-preview"));
            let unknown = json!({"Id":"1", "ImageTags":{"Primary":"new"}, "ImageBlurHashes":{"Primary":{"old":"wrong-preview"}}});
            assert!(
                primary_image_ref(server, "album", "1", &unknown)
                    .unwrap()
                    .blur_hash
                    .is_none()
            );
            let backdrop = json!({"Id":"1", "BackdropImageTags":["back"], "ImageBlurHashes":{"Backdrop":{"back":"back-preview"}}});
            assert_eq!(
                backdrop_image_ref(server, &backdrop)
                    .unwrap()
                    .blur_hash
                    .as_deref(),
                Some("back-preview")
            );
        }
    }
}
