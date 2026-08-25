use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use library::{
    AlbumArtworkFacts, FavoriteItemId, MetadataChange, MetadataEdit, MetadataField, MetadataItem,
    MetadataItemId, MetadataValues, PlaylistId, RadioSeed, StreamQuality, StreamRequest, TrackId,
};
use wiremock::matchers::{body_json, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::CredentialSettingsInput;
use crate::source::PreparedSourceChange;

fn account(base_url: &str, server_id: Option<&str>, user_id: &str) -> JellyfinSourceConfig {
    JellyfinSourceConfig {
        base_url: base_url.to_string(),
        server_id: server_id.map(str::to_string),
        user_id: user_id.to_string(),
        username: "listener".to_string(),
        trust_invalid_cert: false,
        use_instant_mix: false,
    }
}

#[test]
fn account_identity_preserves_legacy_ids_without_merging_users_or_servers() {
    let legacy = account("https://music.example", None, "user-one");
    assert!(
        legacy
            .same_account(&account(
                "https://music.example/",
                Some("server-one"),
                "user-one",
            ))
            .expect("legacy account comparison")
    );

    let current = account("https://old.example", Some("server-one"), "user-one");
    assert!(
        current
            .same_account(&account(
                "https://new.example",
                Some("server-one"),
                "user-one",
            ))
            .expect("server identity comparison")
    );
    assert!(
        !current
            .same_account(&account(
                "https://old.example",
                Some("server-one"),
                "user-two",
            ))
            .expect("user identity comparison")
    );
    assert!(
        !current
            .same_account(&account(
                "https://old.example",
                Some("server-two"),
                "user-one",
            ))
            .expect("server identity comparison")
    );
}

fn provider(server: &MockServer, token: &str) -> JellyfinSource {
    let configuration = crate::config::encode_provider_payload(
        SourceId::new("jellyfin:server:test:user:user-one"),
        JELLYFIN_SOURCE_ID,
        "Jellyfin",
        JellyfinSourceConfig {
            base_url: server.uri(),
            server_id: Some("test".to_string()),
            user_id: "user-one".to_string(),
            username: "listener".to_string(),
            trust_invalid_cert: false,
            use_instant_mix: false,
        }
        .into_payload(),
    );
    open(
        &configuration,
        Some(token.to_string()),
        Some("rufin-install-one".to_string()),
    )
    .expect("open Jellyfin provider")
}

#[tokio::test]
async fn rating_uses_jellyfins_ten_point_value() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/UserItems/track-one/UserData"))
        .and(query_param("userId", "user-one"))
        .and(body_json(serde_json::json!({ "Rating": 7 })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    provider(&server, "secret-token")
        .set_rating(
            FavoriteItemId::Track(TrackId::new("jellyfin:track:track-one")),
            Some(7),
        )
        .await
        .expect("set Jellyfin rating");
}

#[test]
fn fractional_jellyfin_rating_decodes_to_the_nearest_half_star() {
    let track = track_from_item(
        serde_json::from_value::<JellyfinItem>(serde_json::json!({
            "Id": "track-one",
            "Name": "First",
            "Type": "Audio",
            "UserData": { "Rating": 7.4 }
        }))
        .expect("Jellyfin Track with fractional rating"),
    );

    assert_eq!(track.user_rating, Some(7));
}

fn accepted_library(batches: Vec<library::CandidateBatch>) -> Arc<library::Library> {
    let libraries = library::Libraries::memory().expect("open in-memory Library");
    let mut candidate = libraries
        .begin_source_candidate(library::CandidateHeader {
            source_id: SourceId::new("jellyfin:server:test:user:user-one"),
            input_digest: [1; 32],
        })
        .expect("begin Jellyfin candidate");
    for batch in batches {
        candidate.write(batch).expect("write Jellyfin facts");
    }
    candidate
        .finish(
            library::CandidateFinish {
                freshness: None,
                home: library::HomeFacts::Source {
                    sections: Vec::new(),
                },
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Jellyfin library")
        .library
}

const METADATA_TRACK_ID: &str = "11111111111111111111111111111111";
const METADATA_ALBUM_ID: &str = "22222222222222222222222222222222";
const METADATA_ARTIST_ID: &str = "33333333333333333333333333333333";
const METADATA_SECOND_TRACK_ID: &str = "44444444444444444444444444444444";

async fn metadata_editor(server: &MockServer, item_id: &str, external_ids: &[&str]) {
    let external_id_infos = external_ids
        .iter()
        .map(|key| serde_json::json!({ "Key": key }))
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path(format!("/Items/{item_id}/MetadataEditor")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ExternalIdInfos": external_id_infos
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn metadata_track() -> library::Track {
    track_from_item(
        serde_json::from_value::<JellyfinItem>(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Name": "Before",
            "Type": "Audio",
            "Album": "Album",
            "Artists": ["Artist"]
        }))
        .expect("Jellyfin metadata Track"),
    )
}

pub(super) fn metadata_album() -> library::Album {
    album_from_item(
        serde_json::from_value::<JellyfinItem>(serde_json::json!({
            "Id": METADATA_ALBUM_ID,
            "Name": "Album",
            "Type": "MusicAlbum",
            "AlbumArtist": "Artist"
        }))
        .expect("Jellyfin metadata Album"),
    )
}

fn metadata_artist() -> library::Artist {
    artist_from_item(
        serde_json::from_value::<JellyfinItem>(serde_json::json!({
            "Id": METADATA_ARTIST_ID,
            "Name": "Artist",
            "Type": "MusicArtist"
        }))
        .expect("Jellyfin metadata Artist"),
    )
}

#[tokio::test]
async fn metadata_editor_maps_fields_from_the_exact_item() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");
    metadata_editor(
        &server,
        METADATA_TRACK_ID,
        &["MusicBrainzRecording", "MusicBrainzTrack", "UnsupportedId"],
    )
    .await;
    let item = MetadataItem::Track(metadata_track());

    let editing = source
        .metadata_editing(&item)
        .await
        .expect("editable Jellyfin Track");
    assert_eq!(
        editing.fields(),
        [
            MetadataField::Title,
            MetadataField::SortTitle,
            MetadataField::Artist,
            MetadataField::Album,
            MetadataField::AlbumArtist,
            MetadataField::TrackNumber,
            MetadataField::DiscNumber,
            MetadataField::Year,
            MetadataField::Genre,
            MetadataField::Comment,
            MetadataField::LockData,
            MetadataField::MusicBrainzRecordingId,
            MetadataField::MusicBrainzReleaseTrackId,
        ]
    );

    let forbidden_server = MockServer::start().await;
    let forbidden = provider(&forbidden_server, "secret-token");
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}/MetadataEditor")))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&forbidden_server)
        .await;
    assert_eq!(forbidden.metadata_editing(&item).await, None);
}

#[tokio::test]
async fn metadata_menu_capability_uses_the_cached_admin_policy() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");
    let item = MetadataItem::Track(metadata_track());
    assert!(!source.metadata_entry_available(&item));

    Mock::given(method("GET"))
        .and(path("/Users/user-one"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "user-one",
            "Name": "Listener",
            "Policy": { "IsAdministrator": true }
        })))
        .expect(1)
        .mount(&server)
        .await;
    source.refresh_metadata_editing().await;

    assert!(source.metadata_entry_available(&item));
    let mut synthetic = metadata_track();
    synthetic.make_mut().id = TrackId::new("jellyfin:track:not-a-provider-id");
    assert!(!source.metadata_entry_available(&MetadataItem::Track(synthetic)));
}

#[tokio::test]
async fn synthetic_and_non_provider_item_ids_are_rejected_without_a_request() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");
    let mut synthetic_artist = metadata_artist();
    synthetic_artist.id =
        library::ArtistId::new("jellyfin:artist:musicbrainz:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    let mut non_provider_track = metadata_track();
    non_provider_track.make_mut().id = TrackId::new("jellyfin:track:not-a-provider-id");

    assert_eq!(
        source
            .metadata_editing(&MetadataItem::Artist(synthetic_artist.clone()))
            .await,
        None
    );
    assert!(!source.metadata_source_search(&MetadataItem::Artist(synthetic_artist)));
    assert_eq!(
        source
            .metadata_editing(&MetadataItem::Track(non_provider_track))
            .await,
        None
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded Jellyfin requests")
            .is_empty()
    );
}

#[tokio::test]
async fn metadata_editor_fields_cover_jellyfin_albums_and_artists() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");
    metadata_editor(
        &server,
        METADATA_ALBUM_ID,
        &["MusicBrainzAlbum", "MusicBrainzReleaseGroup"],
    )
    .await;
    metadata_editor(&server, METADATA_ARTIST_ID, &["MusicBrainzArtist"]).await;

    let album = source
        .metadata_editing(&MetadataItem::Album(metadata_album()))
        .await
        .expect("editable Jellyfin Album");
    assert!(album.includes(MetadataField::AlbumArtist));
    assert!(album.includes(MetadataField::MusicBrainzAlbumId));
    assert!(!album.includes(MetadataField::TrackNumber));

    let artist = source
        .metadata_editing(&MetadataItem::Artist(metadata_artist()))
        .await
        .expect("editable Jellyfin Artist");
    assert!(artist.includes(MetadataField::MusicBrainzArtistId));
    assert!(artist.includes(MetadataField::LockData));
    assert!(!artist.includes(MetadataField::Album));
}

#[tokio::test]
async fn artist_identification_keeps_the_jellyfin_result_for_save() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Items/RemoteSearch/MusicArtist"))
        .and(body_json(serde_json::json!({
            "ItemId": METADATA_ARTIST_ID,
            "SearchInfo": {
                "Name": "Artist",
                "Year": null,
                "ProviderIds": {}
            }
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "Name": "Identified artist",
                "Overview": "Identified overview",
                "ProviderIds": {
                    "MusicBrainzArtist": "identified-artist-id"
                }
            }])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let identified = source
        .identify_metadata(
            &MetadataItem::Artist(metadata_artist()),
            &MetadataValues {
                title: "Artist".to_string(),
                ..MetadataValues::default()
            },
        )
        .await
        .expect("identify Jellyfin artist")
        .expect("Jellyfin artist remote search candidate");

    assert_eq!(identified.values.title, "Identified artist");
    assert_eq!(
        identified.values.comment.as_deref(),
        Some("Identified overview")
    );
    assert_eq!(
        identified.values.musicbrainz_artist_id.as_deref(),
        Some("identified-artist-id")
    );
    assert!(identified.application.is_some());
}

#[tokio::test]
async fn album_identification_selects_the_exact_provider_match() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Items/RemoteSearch/MusicAlbum"))
        .and(body_json(serde_json::json!({
            "ItemId": METADATA_ALBUM_ID,
            "SearchInfo": {
                "Name": "Album",
                "Year": 1999,
                "AlbumArtists": ["Expected artist"],
                "ProviderIds": {
                    "MusicBrainzAlbum": "expected-release-id"
                }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "Name": "Album",
                "ProductionYear": 1999,
                "ProviderIds": {
                    "MusicBrainzAlbum": "other-release-id"
                }
            },
            {
                "Name": "Identified album",
                "ProductionYear": 2000,
                "Overview": "Identified overview",
                "AlbumArtist": { "Name": "Identified album artist" },
                "ProviderIds": {
                    "MusicBrainzAlbum": "expected-release-id",
                    "MusicBrainzReleaseGroup": "identified-release-group-id"
                }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let identified = source
        .identify_metadata(
            &MetadataItem::Album(metadata_album()),
            &MetadataValues {
                title: "Album".to_string(),
                year: Some(1999),
                album_artist: Some("Expected artist".to_string()),
                musicbrainz_album_id: Some("expected-release-id".to_string()),
                ..MetadataValues::default()
            },
        )
        .await
        .expect("identify Jellyfin album")
        .expect("Jellyfin album remote search candidate");

    assert_eq!(identified.values.title, "Identified album");
    assert_eq!(identified.values.year, Some(2000));
    assert_eq!(
        identified.values.artist.as_deref(),
        Some("Identified album artist")
    );
    assert_eq!(
        identified.values.album_artist.as_deref(),
        Some("Identified album artist")
    );
    assert_eq!(
        identified.values.musicbrainz_album_id.as_deref(),
        Some("expected-release-id")
    );
    assert_eq!(
        identified.values.musicbrainz_release_group_id.as_deref(),
        Some("identified-release-group-id")
    );
}

#[tokio::test]
async fn identified_album_save_applies_the_source_result_without_a_flat_metadata_post() {
    let server = MockServer::start().await;
    let remote_result = serde_json::json!({
        "Name": "Album",
        "AlbumArtist": { "Name": "Canonical artist" },
        "ProviderIds": {
            "MusicBrainzAlbum": "11111111-2222-3333-4444-555555555555"
        }
    });
    Mock::given(method("POST"))
        .and(path("/Items/RemoteSearch/MusicAlbum"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([remote_result.clone()])),
        )
        .expect(1)
        .mount(&server)
        .await;
    metadata_editor(
        &server,
        METADATA_ALBUM_ID,
        &["MusicBrainzAlbum", "MusicBrainzReleaseGroup"],
    )
    .await;
    let reads = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_ALBUM_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with({
            let reads = Arc::clone(&reads);
            move |_: &wiremock::Request| {
                let body = if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    serde_json::json!({
                        "Id": METADATA_ALBUM_ID,
                        "Etag": "before-revision",
                        "Name": "Album",
                        "AlbumArtists": [{ "Name": "Previous artist", "Id": "old-artist" }]
                    })
                } else {
                    serde_json::json!({
                        "Id": METADATA_ALBUM_ID,
                        "Etag": "after-revision",
                        "Name": "Album",
                        "AlbumArtists": [{ "Name": "Canonical artist", "Id": "canonical-artist" }],
                        "ProviderIds": {
                            "MusicBrainzAlbum": "11111111-2222-3333-4444-555555555555"
                        }
                    })
                };
                ResponseTemplate::new(200).set_body_json(body)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("AlbumIds", METADATA_ALBUM_ID))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Items": [],
            "TotalRecordCount": 0
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/Items/RemoteSearch/Apply/{METADATA_ALBUM_ID}"
        )))
        .and(query_param("ReplaceAllImages", "false"))
        .and(body_json(remote_result))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", METADATA_ALBUM_ID))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": METADATA_ALBUM_ID,
                "Name": "Album",
                "Type": "MusicAlbum",
                "AlbumArtists": [{ "Name": "Canonical artist", "Id": "canonical-artist" }]
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let album = metadata_album();
    let identified = source
        .identify_metadata(
            &MetadataItem::Album(album.clone()),
            &MetadataValues {
                title: "Album".to_string(),
                album_artist: Some("Previous artist".to_string()),
                ..MetadataValues::default()
            },
        )
        .await
        .expect("identify Jellyfin album")
        .expect("Jellyfin album candidate");

    let raw_ids = source
        .write_metadata(
            &MetadataItem::Album(album.clone()),
            &MetadataEdit {
                item_id: MetadataItemId::Album(album.id.clone()),
                revision: Some("etag:before-revision".to_string()),
                application: identified.application,
                changes: Vec::new(),
            },
        )
        .await
        .expect("apply identified Jellyfin metadata");

    assert_eq!(raw_ids, [METADATA_ALBUM_ID]);
    assert_eq!(reads.load(Ordering::SeqCst), 2);

    let library = accepted_library(vec![library::CandidateBatch::Albums(vec![album])]);
    let prepared = source
        .prepare_change(&library, raw_ids.into_iter().collect(), BTreeSet::new())
        .await
        .expect("prepare the identified Jellyfin album");
    let PreparedSourceChange::SourceUpdate(update) = prepared else {
        panic!("an identified Album must remain an exact source update");
    };
    assert_eq!(update.albums.len(), 1);
    assert_eq!(
        update.albums[0].relations.album_artists[0].name,
        "Canonical artist"
    );
    assert_eq!(
        update.albums[0].relations.album_artists[0].id.as_str(),
        "jellyfin:artist:canonical-artist"
    );
}

#[tokio::test]
async fn empty_identification_result_is_no_candidate() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Items/RemoteSearch/MusicArtist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let values = MetadataValues {
        title: "Unknown artist".to_string(),
        ..MetadataValues::default()
    };

    let identified = source
        .identify_metadata(&MetadataItem::Artist(metadata_artist()), &values)
        .await
        .expect("empty Jellyfin identification is not an error");

    assert_eq!(identified, None);
}

#[tokio::test]
async fn metadata_source_search_is_an_item_capability() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");

    assert!(!source.metadata_source_search(&MetadataItem::Track(metadata_track())));
    assert!(source.metadata_source_search(&MetadataItem::Album(metadata_album())));
    assert!(source.metadata_source_search(&MetadataItem::Artist(metadata_artist())));
}

#[tokio::test]
async fn failed_native_identification_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Items/RemoteSearch/MusicArtist"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    assert_eq!(
        source
            .identify_metadata(
                &MetadataItem::Artist(metadata_artist()),
                &MetadataValues {
                    title: "Artist".to_string(),
                    ..MetadataValues::default()
                },
            )
            .await,
        Err("Jellyfin could not search for metadata.".to_string())
    );
}

#[tokio::test]
async fn metadata_write_preserves_fields_required_by_jellyfin_updates() {
    let server = MockServer::start().await;
    metadata_editor(&server, METADATA_TRACK_ID, &[]).await;
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Etag": "before-revision",
            "Name": "Before",
            "ForcedSortName": "Keep sort name",
            "OriginalTitle": "Keep original title",
            "CustomRating": "Keep custom rating",
            "LockData": true,
            "LockedFields": ["Name"],
            "PreferredMetadataCountryCode": "JP",
            "PreferredMetadataLanguage": "ja",
            "IndexNumber": 2,
            "ParentIndexNumber": 1,
            "ProductionYear": 2024,
            "Overview": "Before comment",
            "Artists": ["Resolved artist", "Pending artist"],
            "ArtistItems": [{ "Name": "Resolved artist", "Id": "resolved-id" }],
            "ProviderIds": { "MusicBrainzTrack": "release-track-id" },
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}")))
        .and(body_json(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Etag": "before-revision",
            "Name": "After",
            "ForcedSortName": "Keep sort name",
            "OriginalTitle": "Keep original title",
            "CustomRating": "Keep custom rating",
            "LockData": true,
            "LockedFields": ["Name"],
            "PreferredMetadataCountryCode": "JP",
            "PreferredMetadataLanguage": "ja",
            "IndexNumber": 7,
            "ParentIndexNumber": 1,
            "ProductionYear": 2024,
            "Overview": null,
            "Artists": ["Resolved artist", "Pending artist"],
            "ArtistItems": [
                { "Name": "Resolved artist" },
                { "Name": "Pending artist" }
            ],
            "ProviderIds": { "MusicBrainzTrack": "release-track-id" },
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let track = metadata_track();

    let raw_ids = source
        .write_metadata(
            &MetadataItem::Track(track.clone()),
            &MetadataEdit {
                item_id: MetadataItemId::Track(track.id.clone()),
                revision: Some("etag:before-revision".to_string()),
                application: None,
                changes: vec![
                    MetadataChange::Title("After".to_string()),
                    MetadataChange::TrackNumber(Some(7)),
                    MetadataChange::Comment(None),
                ],
            },
        )
        .await
        .expect("write Jellyfin metadata");

    assert_eq!(raw_ids, [METADATA_TRACK_ID]);
}

#[tokio::test]
async fn album_metadata_write_pages_until_the_reported_total() {
    let server = MockServer::start().await;
    metadata_editor(&server, METADATA_ALBUM_ID, &[]).await;
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_ALBUM_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": METADATA_ALBUM_ID,
            "Etag": "before-revision",
            "Name": "Before"
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("UserId", "user-one"))
        .and(query_param("Recursive", "true"))
        .and(query_param("IncludeItemTypes", "Audio"))
        .and(query_param("AlbumIds", METADATA_ALBUM_ID))
        .and(query_param("StartIndex", "0"))
        .and(query_param("Limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Items": [{ "Id": METADATA_SECOND_TRACK_ID }],
            "TotalRecordCount": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("UserId", "user-one"))
        .and(query_param("Recursive", "true"))
        .and(query_param("IncludeItemTypes", "Audio"))
        .and(query_param("AlbumIds", METADATA_ALBUM_ID))
        .and(query_param("StartIndex", "1"))
        .and(query_param("Limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Items": [{ "Id": METADATA_TRACK_ID }],
            "TotalRecordCount": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/Items/{METADATA_ALBUM_ID}")))
        .and(body_json(serde_json::json!({
            "Id": METADATA_ALBUM_ID,
            "Etag": "before-revision",
            "Name": "After"
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let album = metadata_album();

    let raw_ids = source
        .write_metadata(
            &MetadataItem::Album(album.clone()),
            &MetadataEdit {
                item_id: MetadataItemId::Album(album.id.clone()),
                revision: Some("etag:before-revision".to_string()),
                application: None,
                changes: vec![MetadataChange::Title("After".to_string())],
            },
        )
        .await
        .expect("write Jellyfin album metadata");

    assert_eq!(
        raw_ids,
        [
            METADATA_TRACK_ID,
            METADATA_ALBUM_ID,
            METADATA_SECOND_TRACK_ID
        ]
    );
}

#[tokio::test]
async fn metadata_write_rejects_a_changed_jellyfin_etag_before_posting() {
    let server = MockServer::start().await;
    metadata_editor(&server, METADATA_TRACK_ID, &[]).await;
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Etag": "changed-revision",
            "Name": "Changed elsewhere"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let track = metadata_track();

    let error = source
        .write_metadata(
            &MetadataItem::Track(track.clone()),
            &MetadataEdit {
                item_id: MetadataItemId::Track(track.id.clone()),
                revision: Some("etag:original-revision".to_string()),
                application: None,
                changes: vec![MetadataChange::Title("After".to_string())],
            },
        )
        .await
        .expect_err("stale Jellyfin edit");

    assert_eq!(error, library::MetadataError::Conflict);
}

#[tokio::test]
async fn metadata_read_requires_a_nonempty_jellyfin_etag() {
    let server = MockServer::start().await;
    metadata_editor(&server, METADATA_TRACK_ID, &[]).await;
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Etag": " ",
            "Name": "Track"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let item = MetadataItem::Track(metadata_track());
    let editing = source
        .metadata_editing(&item)
        .await
        .expect("editable Jellyfin metadata");

    source
        .read_metadata(&item, editing)
        .await
        .expect_err("Jellyfin metadata without an Etag");
}

#[tokio::test]
async fn metadata_read_maps_music_fields_and_lists() {
    let server = MockServer::start().await;
    metadata_editor(
        &server,
        METADATA_TRACK_ID,
        &["MusicBrainzRecording", "MusicBrainzTrack"],
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/Items/{METADATA_TRACK_ID}")))
        .and(query_param("Fields", super::metadata::METADATA_ITEM_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": METADATA_TRACK_ID,
            "Etag": "mapped-revision",
            "Name": "Mapped title",
            "IndexNumber": 4,
            "ParentIndexNumber": 2,
            "ProductionYear": 2025,
            "Overview": "Mapped comment",
            "ForcedSortName": "Mapped sort title",
            "Artists": ["First artist", "Second artist"],
            "ArtistItems": [
                { "Name": "First artist", "Id": "artist-one" },
                { "Name": "Second artist", "Id": "artist-two" }
            ],
            "Album": "Mapped album",
            "AlbumArtists": [{ "Name": "Album artist", "Id": "album-artist" }],
            "Genres": ["Rock", "Alternative"],
            "LockData": true,
            "ProviderIds": {
                "MusicBrainzRecording": "recording-id",
                "MusicBrainzTrack": "release-track-id"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let mut track = metadata_track();
    track.make_mut().album_artwork = Some(Arc::new(AlbumArtworkFacts {
        local_artwork: None,
        image_ref: None,
        musicbrainz_release_group_id: Some("release-group-id".to_string()),
        musicbrainz_album_id: Some("release-id".to_string()),
        artist: "Album artist".to_string(),
        title: "Mapped album".to_string(),
    }));
    let item = MetadataItem::Track(track);
    let editing = source
        .metadata_editing(&item)
        .await
        .expect("editable Jellyfin metadata");
    let draft = source
        .read_metadata(&item, editing)
        .await
        .expect("read Jellyfin metadata");

    assert_eq!(draft.revision.as_deref(), Some("etag:mapped-revision"));
    assert_eq!(draft.values.title, "Mapped title");
    assert_eq!(
        draft.values.sort_title.as_deref(),
        Some("Mapped sort title")
    );
    assert_eq!(
        draft.values.artist.as_deref(),
        Some("First artist; Second artist")
    );
    assert_eq!(draft.values.album.as_deref(), Some("Mapped album"));
    assert_eq!(draft.values.album_artist.as_deref(), Some("Album artist"));
    assert_eq!(draft.values.track_number, Some(4));
    assert_eq!(draft.values.disc_number, Some(2));
    assert_eq!(draft.values.year, Some(2025));
    assert_eq!(draft.values.comment.as_deref(), Some("Mapped comment"));
    assert_eq!(draft.values.genre.as_deref(), Some("Rock; Alternative"));
    assert_eq!(draft.values.lock_data, Some(true));
    assert_eq!(
        draft.values.musicbrainz_recording_id.as_deref(),
        Some("recording-id")
    );
    assert_eq!(
        draft.values.musicbrainz_release_track_id.as_deref(),
        Some("release-track-id")
    );
    assert_eq!(
        draft.values.musicbrainz_album_id.as_deref(),
        Some("release-id")
    );
    assert_eq!(
        draft.values.musicbrainz_release_group_id.as_deref(),
        Some("release-group-id")
    );
}

fn saved_configuration(
    server: &MockServer,
    name: &str,
    trust_invalid_cert: bool,
) -> SourceConfiguration {
    crate::config::encode_provider_payload(
        SourceId::new("configured:jellyfin"),
        JELLYFIN_SOURCE_ID,
        name,
        JellyfinSourceConfig {
            base_url: server.uri(),
            server_id: Some("server-one".to_string()),
            user_id: "user-one".to_string(),
            username: "Listener".to_string(),
            trust_invalid_cert,
            use_instant_mix: false,
        }
        .into_payload(),
    )
}

fn settings_input(
    server: &MockServer,
    name: &str,
    password: &str,
    trust_invalid_cert: bool,
) -> JellyfinSettingsInput {
    JellyfinSettingsInput {
        credentials: CredentialSettingsInput {
            name: name.to_string(),
            base_url: server.uri(),
            username: "Listener".to_string(),
            password: password.to_string(),
            trust_invalid_cert,
        },
        use_instant_mix: false,
    }
}

fn query(items: serde_json::Value) -> serde_json::Value {
    let count = items.as_array().map_or(0, Vec::len);
    serde_json::json!({
        "Items": items,
        "TotalRecordCount": count
    })
}

#[tokio::test]
async fn search_uses_jellyfin_native_artist_album_and_track_queries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Artists"))
        .and(query_param("SearchTerm", "apple"))
        .and(query_param("Limit", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "artist-one",
                "Name": "Apple Trees",
                "Type": "MusicArtist"
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("IncludeItemTypes", "MusicAlbum"))
        .and(query_param("SearchTerm", "apple"))
        .and(query_param("Limit", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "album-one",
                "Name": "Green Fields",
                "Type": "MusicAlbum",
                "AlbumArtist": "Apple Trees"
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("IncludeItemTypes", "Audio"))
        .and(query_param("SearchTerm", "apple"))
        .and(query_param("Limit", "9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "track-one",
                "Name": "Orchard Walk",
                "Type": "Audio",
                "Album": "Green Fields",
                "Artists": ["Apple Trees"]
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let results = source
        .search(&library::SearchRequest::with_limit("apple", 9))
        .await
        .expect("search Jellyfin");

    assert_eq!(results.artists[0].id.as_str(), "jellyfin:artist:artist-one");
    assert_eq!(results.albums[0].id.as_str(), "jellyfin:album:album-one");
    assert_eq!(results.tracks[0].id.as_str(), "jellyfin:track:track-one");
}

#[tokio::test]
async fn home_refresh_reads_exactly_one_requested_jellyfin_section() {
    let server = MockServer::start().await;
    let cases = [
        (
            SourceHomeSectionKind::MostPlayed,
            "Audio",
            "PlayCount,SortName",
            "track-most",
        ),
        (
            SourceHomeSectionKind::NewlyAdded,
            "MusicAlbum",
            "DateCreated,SortName",
            "album-new",
        ),
        (
            SourceHomeSectionKind::RecentlyPlayed,
            "Audio",
            "DatePlayed,SortName",
            "track-recent",
        ),
        (
            SourceHomeSectionKind::RecentlyReleased,
            "MusicAlbum",
            "ProductionYear,PremiereDate,SortName",
            "album-released",
        ),
    ];
    for (_, item_type, sort_by, id) in cases {
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("IncludeItemTypes", item_type))
            .and(query_param("SortBy", sort_by))
            .and(query_param("SortOrder", "Descending"))
            .and(query_param(
                "Limit",
                library::HOME_SECTION_ITEM_LIMIT.to_string(),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                    "Id": id,
                    "Name": id,
                    "Type": item_type
                }]))),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let source = provider(&server, "secret-token");

    for (kind, item_type, _, id) in cases {
        let section = source
            .read_home_section(kind)
            .await
            .expect("read one Jellyfin Home section");
        assert_eq!(section.kind, kind);
        assert_eq!(section.items.len(), 1);
        let expected = if item_type == "Audio" {
            HomeItemId::Track(TrackId::new(jellyfin_id("track", id)))
        } else {
            HomeItemId::Album(AlbumId::new(jellyfin_id("album", id)))
        };
        assert_eq!(section.items[0], expected);
    }
}

#[tokio::test]
async fn login_uses_server_and_account_identity_with_the_app_device() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(header_regex(
            "authorization",
            "DeviceId=\"rufin-install-one\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "secret-token",
            "ServerId": "server-one",
            "User": {
                "Id": "user-one",
                "Name": "Listener"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ServerName": "Music Box"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let connected = connect(JellyfinSetupInput {
        credentials: CredentialHostInput {
            server_name: None,
            server_url: format!("{}/", server.uri()),
            username: "submitted-name".to_string(),
            password: "secret".to_string(),
            trust_invalid_cert: false,
        },
        use_instant_mix: false,
        device_id: "rufin-install-one".to_string(),
    })
    .await
    .expect("connect Jellyfin provider");

    let (configuration, source, credential) = connected.into_parts();
    assert_eq!(
        configuration.source_id.as_str(),
        "jellyfin:server:server-one:user:user-one"
    );
    assert_eq!(configuration.name, "Music Box");
    let config =
        JellyfinSourceConfig::from_configuration(&configuration).expect("Jellyfin configuration");
    assert_eq!(config.username, "Listener");
    assert_eq!(config.base_url, server.uri());

    assert_eq!(source.source_id(), &configuration.source_id);
    assert_eq!(credential.as_deref(), Some("secret-token"));
    open(
        &configuration,
        credential,
        Some("rufin-install-one".to_string()),
    )
    .expect("reopen Jellyfin provider");
}

#[tokio::test]
async fn name_only_edit_updates_configuration_without_contacting_jellyfin() {
    let server = MockServer::start().await;
    let current = saved_configuration(&server, "Before", false);
    let input = settings_input(&server, "After", "", false);

    let SourceEditResult::ConfigurationOnly(configuration) = edit(
        current.clone(),
        Some("saved-token".to_string()),
        input,
        Some("rufin-install-one".to_string()),
    )
    .await
    .expect("name-only Jellyfin edit") else {
        panic!("a name-only edit must not reopen Jellyfin");
    };

    assert_eq!(configuration.source_id, current.source_id);
    assert_eq!(configuration.name, "After");
}

#[tokio::test]
async fn password_backed_same_account_edit_keeps_the_configured_jellyfin_source() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "new-token",
            "ServerId": "server-one",
            "User": { "Id": "user-one", "Name": "Listener" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ServerName": "Music Box"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let current = saved_configuration(&server, "Before", false);
    let input = settings_input(&server, "After", "new-password", false);

    let SourceEditResult::Connected(connected) = edit(
        current.clone(),
        Some("old-token".to_string()),
        input,
        Some("rufin-install-one".to_string()),
    )
    .await
    .expect("same-account Jellyfin edit") else {
        panic!("the authenticated account must retain the configured source");
    };

    let (configuration, source, credential) = connected.into_parts();
    assert_eq!(configuration.source_id, current.source_id);
    assert_eq!(source.source_id(), &configuration.source_id);
    assert_eq!(credential.as_deref(), Some("new-token"));
}

#[tokio::test]
async fn password_backed_different_account_edit_returns_a_new_jellyfin_source() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "AccessToken": "new-token",
            "ServerId": "server-one",
            "User": { "Id": "user-two", "Name": "Other Listener" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ServerName": "Music Box"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let current = saved_configuration(&server, "Before", false);
    let mut input = settings_input(&server, "After", "new-password", false);
    input.credentials.username = "Other Listener".to_string();

    let SourceEditResult::Connected(connected) = edit(
        current.clone(),
        Some("old-token".to_string()),
        input,
        Some("rufin-install-one".to_string()),
    )
    .await
    .expect("different-account Jellyfin edit") else {
        panic!("a different canonical account must create a new source");
    };

    let (configuration, source, credential) = connected.into_parts();
    assert_ne!(configuration.source_id, current.source_id);
    assert_eq!(
        configuration.source_id.as_str(),
        "jellyfin:server:server-one:user:user-two"
    );
    assert_eq!(source.source_id(), &configuration.source_id);
    assert_eq!(credential.as_deref(), Some("new-token"));
}

#[tokio::test]
async fn trust_only_edit_reopens_jellyfin_from_the_saved_credential_without_network() {
    let server = MockServer::start().await;
    let current = saved_configuration(&server, "Before", false);
    let input = settings_input(&server, "Before", "", true);

    let SourceEditResult::Connected(connected) = edit(
        current.clone(),
        Some("saved-token".to_string()),
        input,
        Some("rufin-install-one".to_string()),
    )
    .await
    .expect("trust-only Jellyfin edit") else {
        panic!("a trust-only edit must reopen the saved Jellyfin source");
    };

    let (configuration, source, credential) = connected.into_parts();
    assert_eq!(configuration.source_id, current.source_id);
    assert!(
        JellyfinSourceConfig::from_configuration(&configuration)
            .expect("Jellyfin configuration")
            .trust_invalid_cert
    );
    assert_eq!(source.source_id(), &current.source_id);
    assert_eq!(credential, None);
}

#[test]
fn sparse_tracks_keep_the_relationships_the_server_did_provide() {
    let item = serde_json::from_value::<JellyfinItem>(serde_json::json!({
        "Id": "track-one",
        "Name": "First",
        "Type": "Audio",
        "AlbumId": "album-missing-from-this-response",
        "Album": "Blue Rooms",
        "Artists": ["Astral Kin"],
        "ArtistItems": [{ "Id": "artist-one", "Name": "Astral Kin" }],
        "GenreItems": [{ "Id": "genre-one", "Name": "Ambient" }],
        "AlbumPrimaryImageTag": "album-cover"
    }))
    .expect("Jellyfin track");
    let track = track_from_item(item);

    assert_eq!(
        track.album_id.as_ref().map(|id| id.as_str()),
        Some("jellyfin:album:album-missing-from-this-response")
    );
    assert_eq!(
        track.relations.artists[0].id.as_str(),
        "jellyfin:artist:artist-one"
    );
    assert_eq!(
        track.relations.genres[0].id.as_str(),
        "jellyfin:genre:genre-one"
    );
    assert_eq!(
        track.image_ref.as_ref().map(|image| image.item_id.as_str()),
        Some("jellyfin:album:album-missing-from-this-response")
    );
}

#[test]
fn jellyfin_musicbrainz_track_is_the_release_track_identity() {
    let item = serde_json::from_value::<JellyfinItem>(serde_json::json!({
        "Id": "track-one",
        "Name": "First",
        "Type": "Audio",
        "ProviderIds": {
            "MusicBrainzRecording": "recording-id",
            "MusicBrainzTrack": "release-track-id"
        }
    }))
    .expect("Jellyfin track");
    let track = track_from_item(item);

    assert_eq!(
        track.musicbrainz_recording_id.as_deref(),
        Some("recording-id")
    );
    assert_eq!(
        track.musicbrainz_release_track_id.as_deref(),
        Some("release-track-id")
    );
}

#[tokio::test]
async fn music_folders_keep_the_jellyfin_library_cover() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Users/user-one/Views"))
        .and(query_param("IncludeExternalContent", "false"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "music-library",
                "Name": "Music",
                "CollectionType": "music",
                "ImageTags": { "Primary": "library-cover" }
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let folders = source
        .read_music_folders()
        .await
        .expect("read Jellyfin music folders");
    let image = folders[0]
        .image_ref
        .as_ref()
        .expect("Jellyfin library cover");

    assert_eq!(image.item_id, "jellyfin:music-folder:music-library");
    assert_eq!(image.tag.as_deref(), Some("library-cover"));
}

#[tokio::test]
async fn audio_pages_request_the_jellyfin_field_that_returns_typed_genres() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("IncludeItemTypes", "Audio"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "track-one",
                "Name": "First",
                "Type": "Audio",
                "GenreItems": [{ "Id": "genre-one", "Name": "Ambient" }]
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let page = source
        .item_page("Audio", 0, 500)
        .await
        .expect("read Jellyfin Audio page");
    let requests = server
        .received_requests()
        .await
        .expect("record Jellyfin request");
    let fields = requests[0]
        .url
        .query_pairs()
        .find_map(|(key, value)| (key == "Fields").then(|| value.into_owned()))
        .expect("Jellyfin Fields query");
    let requested = fields.split(',').collect::<BTreeSet<_>>();
    let track = track_from_item(page.items.into_iter().next().expect("Jellyfin Track"));

    assert!(requested.contains("Genres"));
    assert!(!requested.contains("GenreItems"));
    assert_eq!(
        track.relations.genres[0].id.as_str(),
        "jellyfin:genre:genre-one"
    );
}

#[tokio::test]
async fn exact_track_change_also_acquires_its_referenced_album() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", "track-one"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "track-one",
                "Name": "First",
                "Type": "Audio",
                "AlbumId": "album-one",
                "Album": "Blue Rooms",
                "Artists": ["Astral Kin"]
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", "album-one"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "album-one",
                "Name": "Blue Rooms",
                "Type": "MusicAlbum",
                "AlbumArtist": "Astral Kin"
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let library = accepted_library(Vec::new());

    let change = source
        .prepare_change(
            &library,
            BTreeSet::from(["track-one".to_string()]),
            BTreeSet::new(),
        )
        .await
        .expect("read exact Jellyfin change");
    let PreparedSourceChange::SourceUpdate(update) = change else {
        panic!("a resolvable Track change must remain exact");
    };

    assert_eq!(update.tracks.len(), 1);
    assert_eq!(update.albums.len(), 1);
    assert_eq!(update.tracks[0].album_id, Some(update.albums[0].id.clone()));
}

#[tokio::test]
async fn artist_changes_use_the_full_dual_access_normalization_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", "artist-one"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "artist-one",
                "Name": "Example Artist",
                "Type": "MusicArtist"
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let library = accepted_library(vec![library::CandidateBatch::Artists(vec![
        artist_from_item(
            serde_json::from_value::<JellyfinItem>(serde_json::json!({
                "Id": "artist-one",
                "Name": "Example Artist",
                "Type": "MusicArtist"
            }))
            .expect("Jellyfin Artist"),
        ),
    ])]);

    let change = source
        .prepare_change(
            &library,
            BTreeSet::from(["artist-one".to_string()]),
            BTreeSet::new(),
        )
        .await
        .expect("prepare Jellyfin Artist change");

    assert!(matches!(change, PreparedSourceChange::Full));
}

#[tokio::test]
async fn removals_use_the_accepted_library_without_fetching_items() {
    let server = MockServer::start().await;
    let source = provider(&server, "secret-token");
    let track = metadata_track();
    let track_id = track.id.clone();
    let playlist_id = PlaylistId::new("jellyfin:playlist:playlist-one");
    let library = accepted_library(vec![
        library::CandidateBatch::Albums(vec![metadata_album()]),
        library::CandidateBatch::Artists(vec![metadata_artist()]),
        library::CandidateBatch::Tracks(vec![track]),
        library::CandidateBatch::Playlists(vec![library::PlaylistSnapshot {
            playlist: library::Playlist {
                id: playlist_id.clone(),
                name: "Late Set".to_string(),
                image_ref: None,
            },
            entries: Vec::new(),
        }]),
    ]);

    let exact = source
        .prepare_change(
            &library,
            BTreeSet::new(),
            BTreeSet::from([METADATA_TRACK_ID.to_string(), "playlist-one".to_string()]),
        )
        .await
        .expect("resolve exact Jellyfin removals");
    let PreparedSourceChange::SourceUpdate(exact) = exact else {
        panic!("accepted Track and Playlist removals must remain exact");
    };
    assert_eq!(exact.removed_tracks, vec![track_id]);
    assert_eq!(exact.removed_playlists, vec![playlist_id]);

    assert!(matches!(
        source
            .prepare_change(
                &library,
                BTreeSet::new(),
                BTreeSet::from([METADATA_ALBUM_ID.to_string()]),
            )
            .await
            .expect("resolve Album removal"),
        PreparedSourceChange::Full
    ));
    assert!(matches!(
        source
            .prepare_change(
                &library,
                BTreeSet::new(),
                BTreeSet::from([METADATA_ARTIST_ID.to_string()]),
            )
            .await
            .expect("resolve Artist removal"),
        PreparedSourceChange::Full
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("record Jellyfin requests")
            .is_empty()
    );
}

#[tokio::test]
async fn radio_falls_back_from_empty_similar_tracks_to_instant_mix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/track-one/Similar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(query(serde_json::json!([]))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Songs/track-one/InstantMix"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(query(serde_json::json!([{
                "Id": "track-two",
                "Name": "Second",
                "Type": "Audio",
                "Artists": ["Astral Kin"]
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let tracks = source
        .generated_tracks(
            &RadioSeed::Track(TrackId::new("jellyfin:track:track-one")),
            20,
        )
        .await
        .expect("Jellyfin radio");

    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id.as_str(), "jellyfin:track:track-two");
}

#[tokio::test]
async fn playback_report_maps_the_canonical_fact_at_the_jellyfin_boundary() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .and(body_json(serde_json::json!({
            "CanSeek": true,
            "ItemId": "track-one",
            "IsPaused": true,
            "IsMuted": false,
            "PositionTicks": 900000000,
            "VolumeLevel": 63,
            "PlayMethod": "DirectPlay",
            "RepeatMode": "RepeatAll",
            "PlaybackOrder": "Shuffle",
            "Failed": true
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    provider(&server, "secret-token")
        .report_playback(SourceReportFact {
            run: playback::RunId::new(1),
            source_id: SourceId::new("jellyfin:server:test:user:user-one"),
            track_id: TrackId::new("jellyfin:track:track-one"),
            phase: SourceReportPhase::Progress,
            started_at_unix_seconds: 1_700_000_000,
            position_millis: 90_999,
            paused: true,
            muted: false,
            volume: 0.625,
            shuffle: true,
            repeat_mode: playback::RepeatMode::All,
            failed: true,
        })
        .await
        .expect("report Jellyfin playback");
}

#[tokio::test]
async fn playlist_readback_preserves_duplicate_tracks_as_distinct_occurrences() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/playlist-one"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "playlist-one",
            "Name": "Late Set",
            "Type": "Playlist",
            "ChildCount": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Playlists/playlist-one/Items"))
        .and(query_param("StartIndex", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "TotalRecordCount": 2,
            "Items": [{
                "Id": "track-one",
                "Type": "Audio",
                "PlaylistItemId": "entry-one"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Playlists/playlist-one/Items"))
        .and(query_param("StartIndex", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "TotalRecordCount": 2,
            "Items": [{
                "Id": "track-one",
                "Type": "Audio",
                "PlaylistItemId": "entry-two"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");

    let snapshot = source
        .read_playlist(&PlaylistId::new("jellyfin:playlist:playlist-one"))
        .await
        .expect("Jellyfin playlist");

    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(snapshot.entries[0].track_id, snapshot.entries[1].track_id);
    assert_eq!(snapshot.entries[0].occurrence_id, "entry-one");
    assert_eq!(snapshot.entries[1].occurrence_id, "entry-two");
}

#[tokio::test]
async fn playlist_readback_continues_when_the_reported_total_grows() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/playlist-one"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Id": "playlist-one",
            "Name": "Late Set",
            "Type": "Playlist",
            "ChildCount": 3
        })))
        .expect(1)
        .mount(&server)
        .await;
    for (offset, total, entry_id) in [
        (0, 2, "entry-one"),
        (1, 3, "entry-two"),
        (2, 3, "entry-three"),
    ] {
        Mock::given(method("GET"))
            .and(path("/Playlists/playlist-one/Items"))
            .and(query_param("StartIndex", offset.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "TotalRecordCount": total,
                "Items": [{
                    "Id": format!("track-{offset}"),
                    "Type": "Audio",
                    "PlaylistItemId": entry_id
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let source = provider(&server, "secret-token");

    let snapshot = source
        .read_playlist(&PlaylistId::new("jellyfin:playlist:playlist-one"))
        .await
        .expect("Jellyfin playlist");

    assert_eq!(snapshot.entries.len(), 3);
}

async fn assert_incomplete_playlist_rows_fail(items: serde_json::Value) {
    let server = MockServer::start().await;
    let total = items.as_array().expect("playlist rows").len();
    Mock::given(method("GET"))
        .and(path("/Playlists/playlist-one/Items"))
        .and(query_param("StartIndex", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "TotalRecordCount": total,
            "Items": items
        })))
        .expect(1)
        .mount(&server)
        .await;
    let source = provider(&server, "secret-token");
    let playlist = playlist_from_item(
        serde_json::from_value(serde_json::json!({
            "Id": "playlist-one",
            "Name": "Late Set",
            "Type": "Playlist"
        }))
        .expect("playlist item"),
    );

    let error = source
        .read_playlist_snapshot(playlist)
        .await
        .expect_err("incomplete playlist rows must fail acquisition");

    assert!(error.to_string().contains("incomplete playlist"));
}

#[tokio::test]
async fn library_acquisition_rejects_missing_or_duplicate_playlist_occurrence_ids() {
    assert_incomplete_playlist_rows_fail(serde_json::json!([{
        "Id": "track-one",
        "Type": "Audio"
    }]))
    .await;
    assert_incomplete_playlist_rows_fail(serde_json::json!([{
        "Id": "track-one",
        "Type": "Audio",
        "PlaylistItemId": "entry-one"
    }, {
        "Id": "track-two",
        "Type": "Audio",
        "PlaylistItemId": "entry-one"
    }]))
    .await;
}

#[tokio::test]
async fn stream_keeps_auth_for_playback_and_redacts_it_for_logs() {
    let server = MockServer::start().await;
    let configuration = crate::config::encode_provider_payload(
        SourceId::new("jellyfin:server:test:user:user-one"),
        JELLYFIN_SOURCE_ID,
        "Jellyfin",
        JellyfinSourceConfig {
            base_url: server.uri(),
            server_id: Some("test".to_string()),
            user_id: "user-one".to_string(),
            username: "listener".to_string(),
            trust_invalid_cert: true,
            use_instant_mix: false,
        }
        .into_payload(),
    );
    let source = open(
        &configuration,
        Some("secret-token".to_string()),
        Some("rufin-install-one".to_string()),
    )
    .expect("open Jellyfin provider");
    let stream = source
        .resolve_stream(&StreamRequest::new(
            TrackId::new("jellyfin:track:track-one"),
            StreamQuality::MaxBitrateKbps(320),
        ))
        .await
        .expect("Jellyfin stream");

    assert!(stream.uri().contains("api_key=secret-token"));
    assert!(stream.uri().contains("MaxStreamingBitrate=320000"));
    assert!(stream.uri().contains("TranscodingContainer=mp3"));
    assert!(stream.uri().contains("AudioCodec=mp3"));
    assert!(!stream.redacted_uri().contains("secret-token"));
    assert!(stream.redacted_uri().contains("api_key=%3Credacted%3E"));
    assert!(stream.trust_invalid_certificate());

    let download = source
        .resolve_download(&StreamRequest::new(
            TrackId::new("jellyfin:track:track-one"),
            StreamQuality::MaxBitrateKbps(320),
        ))
        .expect("Jellyfin download");
    assert_eq!(download.transcoded_extension(), Some("ogg"));
    assert!(
        download
            .stream()
            .uri()
            .contains("Audio/track-one/Universal")
    );
    assert!(download.stream().uri().contains("transcodingContainer=ogg"));
    assert!(download.stream().uri().contains("audioCodec=opus"));
    assert!(download.stream().uri().contains("audioBitRate=256000"));
    assert!(download.stream().uri().contains("api_key=secret-token"));
    assert!(!download.stream().redacted_uri().contains("secret-token"));

    let original = source
        .resolve_download(&StreamRequest::original(TrackId::new(
            "jellyfin:track:track-one",
        )))
        .expect("original Jellyfin download");
    assert_eq!(original.transcoded_extension(), None);
    assert!(original.stream().uri().contains("Audio/track-one/stream"));
    assert!(original.stream().uri().contains("Static=true"));
}
