use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use library::{
    MetadataApplication, MetadataChange, MetadataDraft, MetadataEdit, MetadataEditing,
    MetadataError, MetadataField, MetadataIdentification, MetadataItem, MetadataValues,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::client::{JellyfinUser, endpoint};
use super::item::ItemQueryResult;
use super::refresh::PageState;
use super::{JellyfinSource, SourceError, SourceResult};

const TRACK_FIELDS: [MetadataField; 11] = [
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
];
const ALBUM_FIELDS: [MetadataField; 8] = [
    MetadataField::Title,
    MetadataField::SortTitle,
    MetadataField::Artist,
    MetadataField::AlbumArtist,
    MetadataField::Year,
    MetadataField::Genre,
    MetadataField::Comment,
    MetadataField::LockData,
];
const ARTIST_FIELDS: [MetadataField; 5] = [
    MetadataField::Title,
    MetadataField::SortTitle,
    MetadataField::Genre,
    MetadataField::Comment,
    MetadataField::LockData,
];
pub(super) const METADATA_ITEM_FIELDS: &str = "Genres,ProviderIds,AlbumArtists,ArtistItems,Overview,\
ProductionYear,Settings,OriginalTitle,CustomRating,Etag";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MetadataEditorInfo {
    #[serde(default)]
    external_id_infos: Vec<ExternalIdInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ExternalIdInfo {
    key: String,
}

impl JellyfinSource {
    pub(crate) fn metadata_entry_available(&self, item: &MetadataItem) -> bool {
        self.metadata_editing.load(Ordering::Acquire) && item_raw_id(item).is_ok()
    }

    pub(crate) async fn refresh_metadata_editing(&self) {
        let available = match endpoint(&self.base_url, &format!("Users/{}", self.user_id)) {
            Ok(url) => self
                .get_json::<JellyfinUser>(url)
                .await
                .is_ok_and(|user| user.policy.is_administrator),
            Err(_) => false,
        };
        self.metadata_editing.store(available, Ordering::Release);
    }

    pub(crate) async fn metadata_editing(&self, item: &MetadataItem) -> Option<MetadataEditing> {
        let raw_id = item_raw_id(item).ok()?;
        let url = endpoint(&self.base_url, &format!("Items/{raw_id}/MetadataEditor")).ok()?;
        let info = self.get_json::<MetadataEditorInfo>(url).await.ok()?;
        Some(editing(item, &info))
    }

    pub(crate) fn metadata_source_search(&self, item: &MetadataItem) -> bool {
        item_raw_id(item).is_ok()
            && matches!(item, MetadataItem::Album(_) | MetadataItem::Artist(_))
    }

    pub(crate) async fn read_metadata(
        &self,
        item: &MetadataItem,
        editing: MetadataEditing,
    ) -> SourceResult<MetadataDraft> {
        let raw_id = item_raw_id(item)?;
        let value = self.read_metadata_item(raw_id).await?;
        Ok(MetadataDraft {
            item_id: item.id(),
            editing,
            source_search: self.metadata_source_search(item),
            revision: Some(revision(&value)?),
            values: values(&value, item),
            scope: library::MetadataScope::Item,
            mixed_fields: Default::default(),
        })
    }

    pub(crate) async fn identify_metadata(
        &self,
        item: &MetadataItem,
        values: &MetadataValues,
    ) -> Result<Option<MetadataIdentification>, String> {
        if !self.metadata_source_search(item) || values.title.trim().is_empty() {
            return Ok(None);
        }
        let item_type = match item {
            MetadataItem::Album(_) => "MusicAlbum",
            MetadataItem::Artist(_) => "MusicArtist",
            MetadataItem::Track(_) => unreachable!("Jellyfin Audio has no remote Identify API"),
        };
        let raw_id =
            item_raw_id(item).map_err(|_| "Jellyfin could not identify this item.".to_string())?;
        let url = endpoint(&self.base_url, &format!("Items/RemoteSearch/{item_type}"))
            .map_err(|_| "Jellyfin could not identify this item.".to_string())?;
        let mut search_info = json!({
            "Name": values.title,
            "Year": values.year,
            "ProviderIds": identification_provider_ids(item, values),
        });
        if matches!(item, MetadataItem::Album(_)) {
            let album_artists =
                split_values(values.album_artist.as_deref().or(values.artist.as_deref()));
            if !album_artists.is_empty() {
                search_info
                    .as_object_mut()
                    .expect("Jellyfin search info object")
                    .insert("AlbumArtists".to_string(), json!(album_artists));
            }
        }
        let body = json!({
            "ItemId": raw_id,
            "SearchInfo": search_info,
        });
        let results = self
            .send_json::<Vec<Value>>(self.client.post(url).json(&body))
            .await
            .map_err(|_| "Jellyfin could not search for metadata.".to_string())?;
        Ok(select_identification_result(item, values, &results))
    }

    pub(crate) async fn write_metadata(
        &self,
        item: &MetadataItem,
        edit: &MetadataEdit,
    ) -> Result<Vec<String>, MetadataError> {
        if edit.item_id != item.id() {
            return Err(MetadataError::Unavailable);
        }
        let editing = self
            .metadata_editing(item)
            .await
            .ok_or(MetadataError::Unavailable)?;
        edit.validate(&editing)?;
        let raw_id = item_raw_id(item).map_err(source_write_error)?;
        let value = self
            .read_metadata_item(raw_id)
            .await
            .map_err(source_write_error)?;
        let expected_revision = edit.revision.as_deref().ok_or(MetadataError::Conflict)?;
        if revision(&value).map_err(source_write_error)? != expected_revision {
            return Err(MetadataError::Conflict);
        }
        if edit.changes.is_empty() {
            if edit.application.is_none() {
                return Ok(vec![raw_id.to_string()]);
            }
        }
        let refresh_ids = self
            .metadata_refresh_ids(item, raw_id)
            .await
            .map_err(source_write_error)?;

        if let Some(application) = &edit.application {
            let candidate = serde_json::from_str::<Value>(application.as_str()).map_err(|_| {
                MetadataError::Write(
                    "Jellyfin could not apply the selected metadata result.".to_string(),
                )
            })?;
            let mut url = endpoint(
                &self.base_url,
                &format!("Items/RemoteSearch/Apply/{raw_id}"),
            )
            .map_err(source_write_error)?;
            url.query_pairs_mut()
                .append_pair("ReplaceAllImages", "false");
            self.send_unit(self.client.post(url).json(&candidate))
                .await
                .map_err(source_write_error)?;

            let mut value = self
                .read_metadata_item(raw_id)
                .await
                .map_err(|error| MetadataError::SavedRefreshFailed(error.to_string()))?;
            let applied_values = values(&value, item);
            let remaining = edit
                .changes
                .iter()
                .filter(|change| !change.matches(&applied_values))
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                return Ok(refresh_ids);
            }
            let object = value.as_object_mut().ok_or_else(|| {
                MetadataError::SavedRefreshFailed(
                    "Jellyfin returned an invalid metadata item.".to_string(),
                )
            })?;
            preserve_complete_artist_items(object);
            for change in remaining {
                apply_change(object, change);
            }
            let url = endpoint(&self.base_url, &format!("Items/{raw_id}"))
                .map_err(|error| MetadataError::SavedRefreshFailed(error.to_string()))?;
            self.send_unit(self.client.post(url).json(&value))
                .await
                .map_err(|error| MetadataError::SavedRefreshFailed(error.to_string()))?;
            return Ok(refresh_ids);
        }

        let mut value = self
            .read_metadata_item(raw_id)
            .await
            .map_err(source_write_error)?;
        if revision(&value).map_err(source_write_error)? != expected_revision {
            return Err(MetadataError::Conflict);
        }
        let object = value.as_object_mut().ok_or_else(|| {
            MetadataError::Write("Jellyfin returned an invalid metadata item.".to_string())
        })?;
        preserve_complete_artist_items(object);
        for change in &edit.changes {
            apply_change(object, change);
        }
        let url =
            endpoint(&self.base_url, &format!("Items/{raw_id}")).map_err(source_write_error)?;
        self.send_unit(self.client.post(url).json(&value))
            .await
            .map_err(source_write_error)?;
        Ok(refresh_ids)
    }

    async fn read_metadata_item(&self, raw_id: &str) -> SourceResult<Value> {
        let mut url = endpoint(&self.base_url, &format!("Items/{raw_id}"))?;
        url.query_pairs_mut()
            .append_pair("Fields", METADATA_ITEM_FIELDS);
        self.get_json(url).await
    }

    async fn metadata_refresh_ids(
        &self,
        item: &MetadataItem,
        raw_id: &str,
    ) -> SourceResult<Vec<String>> {
        let related = match item {
            MetadataItem::Track(_) => None,
            MetadataItem::Album(_) => Some(("AlbumIds", "Audio")),
            MetadataItem::Artist(_) => Some(("ArtistIds", "Audio,MusicAlbum")),
        };
        let mut ids = BTreeSet::from([raw_id.to_string()]);
        let Some((filter, item_types)) = related else {
            return Ok(ids.into_iter().collect());
        };
        let mut pages = PageState::default();
        loop {
            let mut url = endpoint(&self.base_url, "Items")?;
            url.query_pairs_mut()
                .append_pair("UserId", &self.user_id)
                .append_pair("Recursive", "true")
                .append_pair("IncludeItemTypes", item_types)
                .append_pair(filter, raw_id)
                .append_pair("StartIndex", &pages.offset().to_string())
                .append_pair("Limit", &super::COLLECTION_PAGE_SIZE.to_string());
            let response = self.get_json::<ItemQueryResult>(url).await?;
            let returned = response.items.len();
            let finished = pages.advance(returned, response.total_record_count)?;
            ids.extend(response.items.into_iter().map(|item| item.id));
            if finished {
                break;
            }
        }
        Ok(ids.into_iter().collect())
    }
}

fn editing(item: &MetadataItem, info: &MetadataEditorInfo) -> MetadataEditing {
    let (base, provider_fields): (&[MetadataField], &[(&str, MetadataField)]) = match item {
        MetadataItem::Track(_) => (
            TRACK_FIELDS.as_slice(),
            &[
                (
                    "MusicBrainzRecording",
                    MetadataField::MusicBrainzRecordingId,
                ),
                ("MusicBrainzTrack", MetadataField::MusicBrainzReleaseTrackId),
                ("MusicBrainzAlbum", MetadataField::MusicBrainzAlbumId),
                (
                    "MusicBrainzReleaseGroup",
                    MetadataField::MusicBrainzReleaseGroupId,
                ),
            ],
        ),
        MetadataItem::Album(_) => (
            ALBUM_FIELDS.as_slice(),
            &[
                ("MusicBrainzAlbum", MetadataField::MusicBrainzAlbumId),
                (
                    "MusicBrainzReleaseGroup",
                    MetadataField::MusicBrainzReleaseGroupId,
                ),
            ],
        ),
        MetadataItem::Artist(_) => (
            ARTIST_FIELDS.as_slice(),
            &[("MusicBrainzArtist", MetadataField::MusicBrainzArtistId)],
        ),
    };
    let mut fields = base.to_vec();
    fields.extend(provider_fields.iter().filter_map(|(key, field)| {
        info.external_id_infos
            .iter()
            .any(|info| info.key.eq_ignore_ascii_case(key))
            .then_some(*field)
    }));
    MetadataEditing::new(fields)
}

fn identification_provider_ids(item: &MetadataItem, values: &MetadataValues) -> Map<String, Value> {
    let mut ids = Map::new();
    let mut insert = |key: &str, value: Option<&str>| {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            ids.insert(key.to_string(), Value::String(value.to_string()));
        }
    };
    match item {
        MetadataItem::Album(_) => {
            insert("MusicBrainzAlbum", values.musicbrainz_album_id.as_deref());
            insert(
                "MusicBrainzReleaseGroup",
                values.musicbrainz_release_group_id.as_deref(),
            );
        }
        MetadataItem::Artist(_) => {
            insert("MusicBrainzArtist", values.musicbrainz_artist_id.as_deref());
        }
        MetadataItem::Track(_) => {}
    }
    ids
}

fn select_identification_result(
    item: &MetadataItem,
    values: &MetadataValues,
    results: &[Value],
) -> Option<MetadataIdentification> {
    let selected = match results {
        [] => return None,
        [result] => result,
        _ => {
            let best_score = results
                .iter()
                .map(|result| identification_score(item, values, result))
                .max()
                .unwrap_or_default();
            let mut best = results
                .iter()
                .filter(|result| identification_score(item, values, result) == best_score);
            let selected = best.next();
            if best_score == 0 || selected.is_none() || best.next().is_some() {
                return None;
            }
            selected.expect("one highest-scoring Jellyfin result")
        }
    };
    Some(MetadataIdentification::source(
        identified_values(item, values, selected),
        MetadataApplication::new(
            serde_json::to_string(selected).expect("serialize Jellyfin remote search result"),
        ),
    ))
}

fn identification_score(item: &MetadataItem, values: &MetadataValues, result: &Value) -> u8 {
    let exact_id = match item {
        MetadataItem::Album(_) => [
            (
                values.musicbrainz_album_id.as_deref(),
                provider_id(result, "MusicBrainzAlbum"),
            ),
            (
                values.musicbrainz_release_group_id.as_deref(),
                provider_id(result, "MusicBrainzReleaseGroup"),
            ),
        ]
        .into_iter()
        .any(|(expected, actual)| expected.is_some() && expected == actual.as_deref()),
        MetadataItem::Artist(_) => {
            values
                .musicbrainz_artist_id
                .as_deref()
                .is_some_and(|expected| {
                    provider_id(result, "MusicBrainzArtist").as_deref() == Some(expected)
                })
        }
        MetadataItem::Track(_) => false,
    };
    if exact_id {
        return 3;
    }
    let exact_name =
        string(result, "Name").is_some_and(|name| name.eq_ignore_ascii_case(values.title.trim()));
    if !exact_name {
        return 0;
    }
    let matching_year = values
        .year
        .is_some_and(|year| number(result, "ProductionYear") == Some(year));
    if matching_year { 2 } else { 1 }
}

fn identified_values(
    item: &MetadataItem,
    previous: &MetadataValues,
    result: &Value,
) -> MetadataValues {
    let mut values = previous.clone();
    if let Some(title) = string(result, "Name") {
        values.title = title;
    }
    if let Some(year) = number(result, "ProductionYear") {
        values.year = Some(year);
    }
    if let Some(comment) = string(result, "Overview") {
        values.comment = Some(comment);
    }
    match item {
        MetadataItem::Album(_) => {
            let artist = result
                .get("AlbumArtist")
                .and_then(|artist| string(artist, "Name"))
                .or_else(|| named_values(result, "Artists"));
            if artist.is_some() {
                values.artist.clone_from(&artist);
                values.album_artist = artist;
            }
            if let Some(id) = provider_id(result, "MusicBrainzAlbum") {
                values.musicbrainz_album_id = Some(id);
            }
            if let Some(id) = provider_id(result, "MusicBrainzReleaseGroup") {
                values.musicbrainz_release_group_id = Some(id);
            }
        }
        MetadataItem::Artist(_) => {
            if let Some(id) = provider_id(result, "MusicBrainzArtist") {
                values.musicbrainz_artist_id = Some(id);
            }
        }
        MetadataItem::Track(_) => {}
    }
    values
}

fn item_raw_id(item: &MetadataItem) -> SourceResult<&str> {
    let (id, prefix) = match item {
        MetadataItem::Track(track) => (track.id.as_str(), "jellyfin:track:"),
        MetadataItem::Album(album) => (album.id.as_str(), "jellyfin:album:"),
        MetadataItem::Artist(artist) => (artist.id.as_str(), "jellyfin:artist:"),
    };
    let Some(raw_id) = id.strip_prefix(prefix) else {
        return Err(SourceError::InvalidRequest(
            "the metadata item does not belong to Jellyfin",
        ));
    };
    if raw_id.len() != 32 || !raw_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SourceError::InvalidRequest(
            "the metadata item is not a Jellyfin provider item",
        ));
    }
    Ok(raw_id)
}

fn values(value: &Value, item: &MetadataItem) -> MetadataValues {
    let fallback_title = match item {
        MetadataItem::Track(track) => track.title.as_str(),
        MetadataItem::Album(album) => album.title.as_str(),
        MetadataItem::Artist(artist) => artist.name.as_str(),
    };
    let (fallback_release_id, fallback_release_group_id) = match item {
        MetadataItem::Track(track) => track.album_artwork_facts().map_or((None, None), |album| {
            (
                album.musicbrainz_album_id.clone(),
                album.musicbrainz_release_group_id.clone(),
            )
        }),
        MetadataItem::Album(album) => (
            album.musicbrainz_album_id.clone(),
            album.musicbrainz_release_group_id.clone(),
        ),
        MetadataItem::Artist(_) => (None, None),
    };
    MetadataValues {
        title: string(value, "Name").unwrap_or_else(|| fallback_title.to_string()),
        sort_title: string(value, "ForcedSortName"),
        artist: string_values(value, "Artists").or_else(|| named_values(value, "ArtistItems")),
        album: string(value, "Album"),
        album_artist: named_values(value, "AlbumArtists").or_else(|| string(value, "AlbumArtist")),
        track_number: number(value, "IndexNumber"),
        disc_number: number(value, "ParentIndexNumber"),
        year: number(value, "ProductionYear"),
        genre: string_values(value, "Genres"),
        comment: string(value, "Overview"),
        lock_data: value.get("LockData").and_then(Value::as_bool),
        musicbrainz_recording_id: provider_id(value, "MusicBrainzRecording"),
        musicbrainz_release_track_id: provider_id(value, "MusicBrainzTrack"),
        musicbrainz_album_id: provider_id(value, "MusicBrainzAlbum").or(fallback_release_id),
        musicbrainz_release_group_id: provider_id(value, "MusicBrainzReleaseGroup")
            .or(fallback_release_group_id),
        musicbrainz_artist_id: provider_id(value, "MusicBrainzArtist"),
        ..MetadataValues::default()
    }
}

fn string(item: &Value, key: &str) -> Option<String> {
    item.get(key).and_then(Value::as_str).and_then(clean)
}

fn string_values(item: &Value, key: &str) -> Option<String> {
    join_values(
        item.get(key)
            .and_then(Value::as_array)?
            .iter()
            .filter_map(Value::as_str),
    )
}

fn named_values(item: &Value, key: &str) -> Option<String> {
    join_values(
        item.get(key)
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|value| value.get("Name"))
            .filter_map(Value::as_str),
    )
}

fn join_values<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let values = values.into_iter().filter_map(clean).collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}

fn split_values(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(';'))
        .filter_map(clean)
        .collect()
}

fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn number(item: &Value, key: &str) -> Option<u16> {
    item.get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .map(|value| value.min(u64::from(u16::MAX)) as u16)
}

fn provider_id(item: &Value, key: &str) -> Option<String> {
    item.get("ProviderIds")
        .and_then(|ids| ids.get(key))
        .and_then(Value::as_str)
        .and_then(clean)
}

fn apply_change(item: &mut Map<String, Value>, change: &MetadataChange) {
    match change {
        MetadataChange::Title(value) => set_required_string(item, "Name", value),
        MetadataChange::SortTitle(value) => set_string(item, "ForcedSortName", value.as_deref()),
        MetadataChange::Artist(value) => set_named_values(item, "ArtistItems", value.as_deref()),
        MetadataChange::Album(value) => set_string(item, "Album", value.as_deref()),
        MetadataChange::AlbumArtist(value) => {
            set_named_values(item, "AlbumArtists", value.as_deref())
        }
        MetadataChange::TrackNumber(value) => set_number(item, "IndexNumber", *value),
        MetadataChange::DiscNumber(value) => set_number(item, "ParentIndexNumber", *value),
        MetadataChange::Year(value) => set_number(item, "ProductionYear", *value),
        MetadataChange::Genre(value) => set_string_values(item, "Genres", value.as_deref()),
        MetadataChange::Comment(value) => set_string(item, "Overview", value.as_deref()),
        MetadataChange::LockData(value) => {
            item.insert("LockData".to_string(), Value::Bool(*value));
        }
        MetadataChange::MusicBrainzRecordingId(value) => {
            set_provider_id(item, "MusicBrainzRecording", value.as_deref())
        }
        MetadataChange::MusicBrainzReleaseTrackId(value) => {
            set_provider_id(item, "MusicBrainzTrack", value.as_deref())
        }
        MetadataChange::MusicBrainzAlbumId(value) => {
            set_provider_id(item, "MusicBrainzAlbum", value.as_deref())
        }
        MetadataChange::MusicBrainzReleaseGroupId(value) => {
            set_provider_id(item, "MusicBrainzReleaseGroup", value.as_deref())
        }
        MetadataChange::MusicBrainzArtistId(value) => {
            set_provider_id(item, "MusicBrainzArtist", value.as_deref())
        }
        MetadataChange::Bpm(_) => {}
        MetadataChange::Lyrics(_) => {}
    }
}

fn set_required_string(item: &mut Map<String, Value>, key: &str, value: &str) {
    item.insert(key.to_string(), Value::String(value.trim().to_string()));
}

fn set_string(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    item.insert(
        key.to_string(),
        value
            .and_then(clean)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
}

fn set_number(item: &mut Map<String, Value>, key: &str, value: Option<u16>) {
    item.insert(
        key.to_string(),
        value
            .map(|value| Value::from(u64::from(value)))
            .unwrap_or(Value::Null),
    );
}

fn set_string_values(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    item.insert(
        key.to_string(),
        Value::Array(split_values(value).into_iter().map(Value::String).collect()),
    );
}

fn preserve_complete_artist_items(item: &mut Map<String, Value>) {
    let artists = item
        .get("Artists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(clean)
        .collect::<Vec<_>>();
    if artists.is_empty() {
        return;
    }
    let resolved = item
        .get("ArtistItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artist| artist.get("Name"))
        .filter_map(Value::as_str)
        .filter_map(clean)
        .collect::<Vec<_>>();
    if artists != resolved {
        item.insert(
            "ArtistItems".to_string(),
            Value::Array(
                artists
                    .into_iter()
                    .map(|name| json!({ "Name": name }))
                    .collect(),
            ),
        );
    }
}

fn set_named_values(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    item.insert(
        key.to_string(),
        Value::Array(
            split_values(value)
                .into_iter()
                .map(|name| json!({ "Name": name }))
                .collect(),
        ),
    );
}

fn set_provider_id(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    let ids = item
        .entry("ProviderIds".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !ids.is_object() {
        *ids = Value::Object(Map::new());
    }
    set_provider_value(
        ids.as_object_mut()
            .expect("normalized Jellyfin ProviderIds"),
        key,
        value,
    );
}

fn set_provider_value(ids: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.and_then(clean) {
        ids.insert(key.to_string(), Value::String(value));
    } else {
        ids.remove(key);
    }
}

fn revision(item: &Value) -> SourceResult<String> {
    string(item, "Etag")
        .map(|etag| format!("etag:{etag}"))
        .ok_or_else(|| {
            SourceError::Other("Jellyfin did not return an Etag for this metadata item".to_string())
        })
}

#[cfg(test)]
mod artist_preservation_tests {
    use super::*;

    #[test]
    fn complete_artist_names_keep_matching_jellyfin_artist_ids() {
        let original = json!([
            { "Name": "First artist", "Id": "artist-one" },
            { "Name": "Second artist", "Id": "artist-two" }
        ]);
        let mut item = json!({
            "Artists": ["First artist", "Second artist"],
            "ArtistItems": original.clone()
        })
        .as_object()
        .cloned()
        .expect("metadata item");

        preserve_complete_artist_items(&mut item);

        assert_eq!(item["ArtistItems"], original);
    }
}

fn source_write_error(error: SourceError) -> MetadataError {
    MetadataError::Write(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_mapping_writes_the_properties_consumed_by_jellyfin() {
        let mut item = json!({ "ProviderIds": { "Keep": "value" } })
            .as_object()
            .expect("object")
            .clone();
        apply_change(
            &mut item,
            &MetadataChange::SortTitle(Some("Sort".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::Artist(Some("First; Second".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::Genre(Some("Rock; Pop".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::MusicBrainzRecordingId(Some("recording".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::MusicBrainzReleaseTrackId(Some("release-track".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::MusicBrainzAlbumId(Some("release".to_string())),
        );
        apply_change(
            &mut item,
            &MetadataChange::MusicBrainzReleaseGroupId(Some("release-group".to_string())),
        );

        assert_eq!(item["ForcedSortName"], "Sort");
        assert_eq!(item["ArtistItems"][0]["Name"], "First");
        assert_eq!(item["ArtistItems"][1]["Name"], "Second");
        assert_eq!(item["Genres"], json!(["Rock", "Pop"]));
        assert_eq!(item["ProviderIds"]["Keep"], "value");
        assert_eq!(item["ProviderIds"]["MusicBrainzRecording"], "recording");
        assert_eq!(item["ProviderIds"]["MusicBrainzTrack"], "release-track");
        assert_eq!(item["ProviderIds"]["MusicBrainzAlbum"], "release");
        assert_eq!(
            item["ProviderIds"]["MusicBrainzReleaseGroup"],
            "release-group"
        );
    }

    #[test]
    fn identification_ignores_ambiguous_remote_results() {
        let item = MetadataItem::Album(crate::jellyfin::tests::metadata_album());
        let values = MetadataValues {
            title: "Album".to_string(),
            year: Some(2026),
            ..MetadataValues::default()
        };
        let results = json!([
            {
                "Name": "Album",
                "ProductionYear": 2026,
                "ProviderIds": { "MusicBrainzAlbum": "first" }
            },
            {
                "Name": "Album",
                "ProductionYear": 2026,
                "ProviderIds": { "MusicBrainzAlbum": "second" }
            }
        ]);

        assert_eq!(
            select_identification_result(
                &item,
                &values,
                results.as_array().expect("remote results")
            ),
            None
        );
    }
}
