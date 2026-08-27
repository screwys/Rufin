//! Jellyfin metadata reads, writes, and remote identification for exact provider objects.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::client::endpoint;
use super::{JellyfinSource, SourceError, SourceResult};
use crate::{
    AlbumMetadata, AlbumMetadataValues, AlbumMetadataWritable, ArtistMetadata,
    ArtistMetadataValues, ArtistMetadataWritable, SourceMetadataError, TrackMetadata,
    TrackMetadataValues, TrackMetadataWritable,
};

const ITEM_FIELDS: &str = "Genres,ProviderIds,AlbumArtists,ArtistItems,Overview,ProductionYear,Settings,OriginalTitle,CustomRating,Etag";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EditorInfo {
    #[serde(default)]
    external_id_infos: Vec<ExternalId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ExternalId {
    key: String,
}

impl JellyfinSource {
    pub(crate) async fn read_track_metadata(
        &self,
        track: library::TrackRow,
    ) -> Result<TrackMetadata, SourceMetadataError> {
        let raw = raw_id(&track.object_id, "track")?;
        let (item, editor) = self.read_item_and_editor(raw).await?;
        let source_values = track_values(&item, &track);
        let mut values = source_values.clone();
        let mut rufin_filled = crate::TrackMetadataWritable::default();
        if values.musicbrainz_recording_id.is_none() && track.musicbrainz_recording_id.is_some() {
            values.musicbrainz_recording_id = track.musicbrainz_recording_id.clone();
            rufin_filled.musicbrainz_recording_id = true;
        }
        if values.musicbrainz_release_track_id.is_none()
            && track.musicbrainz_release_track_id.is_some()
        {
            values.musicbrainz_release_track_id = track.musicbrainz_release_track_id.clone();
            rufin_filled.musicbrainz_release_track_id = true;
        }
        Ok(TrackMetadata {
            track_key: track.track_key,
            writable: track_writable(&editor),
            source_search: true,
            revision: Some(revision(&item)?),
            source_values,
            values,
            rufin_filled,
        })
    }

    pub(crate) async fn read_album_metadata(
        &self,
        album: library::AlbumRow,
    ) -> Result<AlbumMetadata, SourceMetadataError> {
        let raw = raw_id(&album.object_id, "album")?;
        let (item, editor) = self.read_item_and_editor(raw).await?;
        let source_values = album_values(&item, &album);
        let mut values = source_values.clone();
        let mut rufin_filled = crate::AlbumMetadataWritable::default();
        if values.musicbrainz_album_id.is_none() && album.musicbrainz_release_id.is_some() {
            values.musicbrainz_album_id = album.musicbrainz_release_id.clone();
            rufin_filled.musicbrainz_album_id = true;
        }
        if values.musicbrainz_release_group_id.is_none()
            && album.musicbrainz_release_group_id.is_some()
        {
            values.musicbrainz_release_group_id = album.musicbrainz_release_group_id.clone();
            rufin_filled.musicbrainz_release_group_id = true;
        }
        Ok(AlbumMetadata {
            album_key: album.album_key,
            writable: album_writable(&editor),
            source_search: true,
            revision: Some(revision(&item)?),
            source_values,
            values,
            rufin_filled,
            track_count: album.track_count.max(0) as usize,
            mixed: crate::AlbumMetadataMixed::default(),
        })
    }

    pub(crate) async fn read_artist_metadata(
        &self,
        artist: library::ArtistRow,
    ) -> Result<ArtistMetadata, SourceMetadataError> {
        let raw = raw_id(&artist.object_id, "artist")?;
        let (item, editor) = self.read_item_and_editor(raw).await?;
        let source_values = artist_values(&item, &artist);
        let mut values = source_values.clone();
        let mut rufin_filled = crate::ArtistMetadataWritable::default();
        if values.musicbrainz_artist_id.is_none() && artist.musicbrainz_artist_id.is_some() {
            values.musicbrainz_artist_id = artist.musicbrainz_artist_id.clone();
            rufin_filled.musicbrainz_artist_id = true;
        }
        Ok(ArtistMetadata {
            artist_key: artist.artist_key,
            writable: artist_writable(&editor),
            source_search: true,
            revision: Some(revision(&item)?),
            source_values,
            values,
            rufin_filled,
            track_count: artist.track_count.max(0) as usize,
            mixed: crate::ArtistMetadataMixed::default(),
        })
    }

    async fn read_item_and_editor(
        &self,
        raw: &str,
    ) -> Result<(Value, EditorInfo), SourceMetadataError> {
        let editor = endpoint(&self.base_url, &format!("Items/{raw}/MetadataEditor"))
            .map_err(metadata_write)?;
        let editor = self
            .get_json::<EditorInfo>(editor)
            .await
            .map_err(metadata_write)?;
        let item = self.read_metadata_item(raw).await.map_err(metadata_write)?;
        Ok((item, editor))
    }

    async fn read_metadata_item(&self, raw: &str) -> SourceResult<Value> {
        let mut url = endpoint(&self.base_url, &format!("Items/{raw}"))?;
        url.query_pairs_mut().append_pair("Fields", ITEM_FIELDS);
        self.get_json(url).await
    }

    pub(crate) async fn write_metadata_value(
        &self,
        object_id: &str,
        kind: &str,
        expected_revision: &str,
        application: Option<&str>,
        update: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<String, SourceMetadataError> {
        let raw = raw_id(object_id, kind)?;
        let mut item = self.read_metadata_item(raw).await.map_err(metadata_write)?;
        if revision(&item)? != expected_revision {
            return Err(SourceMetadataError::Conflict);
        }
        if let Some(application) = application {
            let candidate = serde_json::from_str::<Value>(application).map_err(|error| {
                SourceMetadataError::Write(format!("invalid Jellyfin identification: {error}"))
            })?;
            let mut url = endpoint(&self.base_url, &format!("Items/RemoteSearch/Apply/{raw}"))
                .map_err(metadata_write)?;
            url.query_pairs_mut()
                .append_pair("ReplaceAllImages", "false");
            self.send_unit(self.client.post(url).json(&candidate))
                .await
                .map_err(metadata_write)?;
            item = self
                .read_metadata_item(raw)
                .await
                .map_err(|error| SourceMetadataError::SavedRefreshFailed(error.to_string()))?;
        }
        let object = item.as_object_mut().ok_or_else(|| {
            SourceMetadataError::Write("Jellyfin returned an invalid metadata item".to_string())
        })?;
        preserve_complete_artist_items(object);
        update(object);
        let url = endpoint(&self.base_url, &format!("Items/{raw}")).map_err(metadata_write)?;
        self.send_unit(self.client.post(url).json(&item))
            .await
            .map_err(metadata_write)?;
        Ok(raw.to_string())
    }

    pub(crate) async fn identify_album_metadata(
        &self,
        object_id: &str,
        values: &AlbumMetadataValues,
    ) -> Result<Option<(AlbumMetadataValues, String)>, String> {
        let raw = raw_id(object_id, "album").map_err(|error| error.to_string())?;
        let results = self
            .remote_search(raw, "MusicAlbum", &values.title, values.year)
            .await?;
        Ok(select_album_identification(values, &results))
    }

    pub(crate) async fn identify_track_metadata(
        &self,
        object_id: &str,
        values: &TrackMetadataValues,
    ) -> Result<Option<(TrackMetadataValues, String)>, String> {
        let raw = raw_id(object_id, "track").map_err(|error| error.to_string())?;
        let results = self
            .remote_search(raw, "Audio", &values.title, values.year)
            .await?;
        Ok(select_track_identification(values, &results))
    }

    pub(crate) async fn identify_artist_metadata(
        &self,
        object_id: &str,
        values: &ArtistMetadataValues,
    ) -> Result<Option<(ArtistMetadataValues, String)>, String> {
        let raw = raw_id(object_id, "artist").map_err(|error| error.to_string())?;
        let results = self
            .remote_search(raw, "MusicArtist", &values.name, None)
            .await?;
        Ok(select_artist_identification(values, &results))
    }

    async fn remote_search(
        &self,
        raw: &str,
        item_type: &str,
        name: &str,
        year: Option<u16>,
    ) -> Result<Vec<Value>, String> {
        if name.trim().is_empty() {
            return Ok(Vec::new());
        }
        let url = endpoint(&self.base_url, &format!("Items/RemoteSearch/{item_type}"))
            .map_err(|error| error.to_string())?;
        self.send_json(self.client.post(url).json(&json!({
            "ItemId": raw,
            "SearchInfo": { "Name": name, "Year": year }
        })))
        .await
        .map_err(|error| error.to_string())
    }
}

pub(crate) fn apply_track_edit(item: &mut Map<String, Value>, edit: &crate::TrackMetadataEdit) {
    let values = &edit.values;
    let changed = &edit.changed;
    if changed.title {
        set_required(item, "Name", &values.title);
    }
    if changed.sort_title {
        set_string(item, "ForcedSortName", values.sort_title.as_deref());
    }
    if changed.artist {
        set_named_if_changed(item, "ArtistItems", values.artist.as_deref());
    }
    if changed.album {
        set_string(item, "Album", values.album.as_deref());
    }
    if changed.album_artist {
        set_named_if_changed(item, "AlbumArtists", values.album_artist.as_deref());
    }
    if changed.track_number {
        set_number(item, "IndexNumber", values.track_number);
    }
    if changed.disc_number {
        set_number(item, "ParentIndexNumber", values.disc_number);
    }
    if changed.year {
        set_number(item, "ProductionYear", values.year);
    }
    if changed.genre {
        set_strings(item, "Genres", values.genre.as_deref());
    }
    if changed.comment {
        set_string(item, "Overview", values.comment.as_deref());
    }
    if changed.locked
        && let Some(locked) = values.locked
    {
        item.insert("LockData".to_string(), Value::Bool(locked));
    }
    if changed.musicbrainz_recording_id {
        set_provider(
            item,
            "MusicBrainzRecording",
            values.musicbrainz_recording_id.as_deref(),
        );
    }
    if changed.musicbrainz_release_track_id {
        set_provider(
            item,
            "MusicBrainzTrack",
            values.musicbrainz_release_track_id.as_deref(),
        );
    }
    if changed.musicbrainz_album_id {
        set_provider(
            item,
            "MusicBrainzAlbum",
            values.musicbrainz_album_id.as_deref(),
        );
    }
    if changed.musicbrainz_release_group_id {
        set_provider(
            item,
            "MusicBrainzReleaseGroup",
            values.musicbrainz_release_group_id.as_deref(),
        );
    }
    if changed.musicbrainz_artist_id {
        set_provider(
            item,
            "MusicBrainzArtist",
            values.musicbrainz_artist_id.as_deref(),
        );
    }
}

pub(crate) fn apply_album_edit(item: &mut Map<String, Value>, edit: &crate::AlbumMetadataEdit) {
    let values = &edit.values;
    let changed = &edit.changed;
    if changed.title {
        set_required(item, "Name", &values.title);
    }
    if changed.sort_title {
        set_string(item, "ForcedSortName", values.sort_title.as_deref());
    }
    if changed.artist {
        set_named_if_changed(item, "ArtistItems", values.artist.as_deref());
    }
    if changed.album_artist {
        set_named_if_changed(item, "AlbumArtists", values.album_artist.as_deref());
    }
    if changed.year {
        set_number(item, "ProductionYear", values.year);
    }
    if changed.genre {
        set_strings(item, "Genres", values.genre.as_deref());
    }
    if changed.comment {
        set_string(item, "Overview", values.comment.as_deref());
    }
    if changed.locked
        && let Some(locked) = values.locked
    {
        item.insert("LockData".to_string(), Value::Bool(locked));
    }
    if changed.musicbrainz_album_id {
        set_provider(
            item,
            "MusicBrainzAlbum",
            values.musicbrainz_album_id.as_deref(),
        );
    }
    if changed.musicbrainz_release_group_id {
        set_provider(
            item,
            "MusicBrainzReleaseGroup",
            values.musicbrainz_release_group_id.as_deref(),
        );
    }
}

pub(crate) fn apply_artist_edit(item: &mut Map<String, Value>, edit: &crate::ArtistMetadataEdit) {
    let values = &edit.values;
    let changed = &edit.changed;
    if changed.name {
        set_required(item, "Name", &values.name);
    }
    if changed.sort_name {
        set_string(item, "ForcedSortName", values.sort_name.as_deref());
    }
    if changed.genre {
        set_strings(item, "Genres", values.genre.as_deref());
    }
    if changed.comment {
        set_string(item, "Overview", values.comment.as_deref());
    }
    if changed.locked
        && let Some(locked) = values.locked
    {
        item.insert("LockData".to_string(), Value::Bool(locked));
    }
    if changed.musicbrainz_artist_id {
        set_provider(
            item,
            "MusicBrainzArtist",
            values.musicbrainz_artist_id.as_deref(),
        );
    }
}

fn track_values(item: &Value, fallback: &library::TrackRow) -> TrackMetadataValues {
    TrackMetadataValues {
        title: string(item, "Name").unwrap_or_else(|| fallback.title.clone()),
        sort_title: string(item, "ForcedSortName"),
        artist: named(item, "ArtistItems").or_else(|| string_array(item, "Artists")),
        album: string(item, "Album").or_else(|| Some(fallback.display_album.clone())),
        album_artist: named(item, "AlbumArtists"),
        track_number: number(item, "IndexNumber"),
        disc_number: number(item, "ParentIndexNumber"),
        year: number(item, "ProductionYear"),
        genre: string_array(item, "Genres"),
        comment: string(item, "Overview"),
        bpm: fallback.bpm.and_then(|value| u16::try_from(value).ok()),
        locked: boolean(item, "LockData"),
        musicbrainz_recording_id: provider(item, "MusicBrainzRecording"),
        musicbrainz_release_track_id: provider(item, "MusicBrainzTrack"),
        musicbrainz_album_id: provider(item, "MusicBrainzAlbum"),
        musicbrainz_release_group_id: provider(item, "MusicBrainzReleaseGroup"),
        musicbrainz_artist_id: provider(item, "MusicBrainzArtist"),
    }
}

fn album_values(item: &Value, fallback: &library::AlbumRow) -> AlbumMetadataValues {
    AlbumMetadataValues {
        title: string(item, "Name").unwrap_or_else(|| fallback.title.clone()),
        sort_title: string(item, "ForcedSortName"),
        artist: named(item, "ArtistItems").or_else(|| Some(fallback.display_artist.clone())),
        album_artist: named(item, "AlbumArtists").or_else(|| Some(fallback.display_artist.clone())),
        year: number(item, "ProductionYear"),
        genre: string_array(item, "Genres"),
        comment: string(item, "Overview"),
        locked: boolean(item, "LockData"),
        musicbrainz_album_id: provider(item, "MusicBrainzAlbum"),
        musicbrainz_release_group_id: provider(item, "MusicBrainzReleaseGroup"),
    }
}

fn artist_values(item: &Value, fallback: &library::ArtistRow) -> ArtistMetadataValues {
    ArtistMetadataValues {
        name: string(item, "Name").unwrap_or_else(|| fallback.name.clone()),
        sort_name: string(item, "ForcedSortName"),
        genre: string_array(item, "Genres"),
        comment: string(item, "Overview"),
        locked: boolean(item, "LockData"),
        musicbrainz_artist_id: provider(item, "MusicBrainzArtist"),
    }
}

fn track_writable(info: &EditorInfo) -> TrackMetadataWritable {
    TrackMetadataWritable {
        title: true,
        sort_title: true,
        artist: true,
        album: true,
        album_artist: true,
        track_number: true,
        disc_number: true,
        year: true,
        genre: true,
        comment: true,
        bpm: false,
        locked: true,
        musicbrainz_recording_id: external(info, "MusicBrainzRecording"),
        musicbrainz_release_track_id: external(info, "MusicBrainzTrack"),
        musicbrainz_album_id: external(info, "MusicBrainzAlbum"),
        musicbrainz_release_group_id: external(info, "MusicBrainzReleaseGroup"),
        musicbrainz_artist_id: false,
    }
}

fn album_writable(info: &EditorInfo) -> AlbumMetadataWritable {
    AlbumMetadataWritable {
        title: true,
        sort_title: true,
        artist: true,
        album_artist: true,
        year: true,
        genre: true,
        comment: true,
        locked: true,
        musicbrainz_album_id: external(info, "MusicBrainzAlbum"),
        musicbrainz_release_group_id: external(info, "MusicBrainzReleaseGroup"),
    }
}

fn artist_writable(info: &EditorInfo) -> ArtistMetadataWritable {
    ArtistMetadataWritable {
        name: true,
        sort_name: true,
        genre: true,
        comment: true,
        locked: true,
        musicbrainz_artist_id: external(info, "MusicBrainzArtist"),
    }
}

fn external(info: &EditorInfo, key: &str) -> bool {
    info.external_id_infos
        .iter()
        .any(|value| value.key.eq_ignore_ascii_case(key))
}

fn select_album_identification(
    previous: &AlbumMetadataValues,
    results: &[Value],
) -> Option<(AlbumMetadataValues, String)> {
    let exact = results
        .iter()
        .filter(|value| {
            previous
                .musicbrainz_album_id
                .as_deref()
                .is_some_and(|id| provider(value, "MusicBrainzAlbum").as_deref() == Some(id))
                || previous
                    .musicbrainz_release_group_id
                    .as_deref()
                    .is_some_and(|id| {
                        provider(value, "MusicBrainzReleaseGroup").as_deref() == Some(id)
                    })
        })
        .collect::<Vec<_>>();
    let selected = match exact.as_slice() {
        [one] => *one,
        [] => select_result(&previous.title, previous.year, results)?,
        _ => return None,
    };
    let mut values = previous.clone();
    values.title = string(selected, "Name").unwrap_or_else(|| values.title.clone());
    values.year = number(selected, "ProductionYear").or(values.year);
    values.artist = named(selected, "ArtistItems").or(values.artist);
    values.album_artist = named(selected, "AlbumArtists").or(values.album_artist);
    values.musicbrainz_album_id =
        provider(selected, "MusicBrainzAlbum").or(values.musicbrainz_album_id);
    values.musicbrainz_release_group_id =
        provider(selected, "MusicBrainzReleaseGroup").or(values.musicbrainz_release_group_id);
    Some((values, serde_json::to_string(selected).ok()?))
}

fn select_track_identification(
    previous: &TrackMetadataValues,
    results: &[Value],
) -> Option<(TrackMetadataValues, String)> {
    let exact = results
        .iter()
        .filter(|value| {
            previous
                .musicbrainz_recording_id
                .as_deref()
                .is_some_and(|id| provider(value, "MusicBrainzRecording").as_deref() == Some(id))
                || previous
                    .musicbrainz_release_track_id
                    .as_deref()
                    .is_some_and(|id| provider(value, "MusicBrainzTrack").as_deref() == Some(id))
        })
        .collect::<Vec<_>>();
    let selected = match exact.as_slice() {
        [one] => *one,
        [] => select_result(&previous.title, previous.year, results)?,
        _ => return None,
    };
    let mut values = previous.clone();
    values.title = string(selected, "Name").unwrap_or_else(|| values.title.clone());
    values.year = number(selected, "ProductionYear").or(values.year);
    values.artist = named(selected, "ArtistItems")
        .or_else(|| string_array(selected, "Artists"))
        .or(values.artist);
    values.album = string(selected, "Album").or(values.album);
    values.album_artist = named(selected, "AlbumArtists").or(values.album_artist);
    values.musicbrainz_recording_id =
        provider(selected, "MusicBrainzRecording").or(values.musicbrainz_recording_id);
    values.musicbrainz_release_track_id =
        provider(selected, "MusicBrainzTrack").or(values.musicbrainz_release_track_id);
    values.musicbrainz_album_id =
        provider(selected, "MusicBrainzAlbum").or(values.musicbrainz_album_id);
    values.musicbrainz_release_group_id =
        provider(selected, "MusicBrainzReleaseGroup").or(values.musicbrainz_release_group_id);
    Some((values, serde_json::to_string(selected).ok()?))
}

fn select_artist_identification(
    previous: &ArtistMetadataValues,
    results: &[Value],
) -> Option<(ArtistMetadataValues, String)> {
    let exact = results
        .iter()
        .filter(|value| {
            previous
                .musicbrainz_artist_id
                .as_deref()
                .is_some_and(|id| provider(value, "MusicBrainzArtist").as_deref() == Some(id))
        })
        .collect::<Vec<_>>();
    let selected = match exact.as_slice() {
        [one] => *one,
        [] => select_result(&previous.name, None, results)?,
        _ => return None,
    };
    let mut values = previous.clone();
    values.name = string(selected, "Name").unwrap_or_else(|| values.name.clone());
    values.musicbrainz_artist_id =
        provider(selected, "MusicBrainzArtist").or(values.musicbrainz_artist_id);
    Some((values, serde_json::to_string(selected).ok()?))
}

fn select_result<'a>(name: &str, year: Option<u16>, results: &'a [Value]) -> Option<&'a Value> {
    let matches = results
        .iter()
        .filter(|value| {
            string(value, "Name").is_some_and(|value| value.eq_ignore_ascii_case(name.trim()))
                && year.is_none_or(|year| number(value, "ProductionYear") == Some(year))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

fn raw_id<'a>(object_id: &'a str, kind: &str) -> Result<&'a str, SourceMetadataError> {
    object_id
        .strip_prefix(&format!("jellyfin:{kind}:"))
        .filter(|raw| !raw.is_empty())
        .ok_or(SourceMetadataError::Unavailable)
}

fn revision(item: &Value) -> Result<String, SourceMetadataError> {
    string(item, "Etag")
        .map(|value| format!("etag:{value}"))
        .ok_or_else(|| SourceMetadataError::Write("Jellyfin returned no metadata Etag".to_string()))
}

fn metadata_write(error: SourceError) -> SourceMetadataError {
    SourceMetadataError::Write(error.to_string())
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).and_then(clean)
}
fn boolean(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}
fn number(value: &Value, key: &str) -> Option<u16> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}
fn provider(value: &Value, key: &str) -> Option<String> {
    value
        .pointer(&format!("/ProviderIds/{key}"))
        .and_then(Value::as_str)
        .and_then(clean)
}
fn named(value: &Value, key: &str) -> Option<String> {
    let values = value
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|value| value.get("Name").and_then(Value::as_str))
        .filter_map(clean)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}
fn string_array(value: &Value, key: &str) -> Option<String> {
    let values = value
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter_map(clean)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}
fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}
fn split(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split([';', ',']))
        .filter_map(clean)
        .collect()
}
fn set_required(item: &mut Map<String, Value>, key: &str, value: &str) {
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
        value.map(Value::from).unwrap_or(Value::Null),
    );
}
fn set_strings(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    item.insert(
        key.to_string(),
        Value::Array(split(value).into_iter().map(Value::String).collect()),
    );
}
fn set_named(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    item.insert(
        key.to_string(),
        Value::Array(
            split(value)
                .into_iter()
                .map(|name| json!({"Name":name}))
                .collect(),
        ),
    );
}
fn set_named_if_changed(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    let current = Value::Object(item.clone());
    let desired = split(value).join("; ");
    if named(&current, key).unwrap_or_default() != desired {
        set_named(item, key, value);
    }
}
fn set_provider(item: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    let ids = item
        .entry("ProviderIds".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !ids.is_object() {
        *ids = Value::Object(Map::new());
    }
    let ids = ids.as_object_mut().expect("normalized provider IDs");
    if let Some(value) = value.and_then(clean) {
        ids.insert(key.to_string(), Value::String(value));
    } else {
        ids.remove(key);
    }
}
fn preserve_complete_artist_items(item: &mut Map<String, Value>) {
    let names = item
        .get("Artists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(clean)
        .collect::<Vec<_>>();
    let resolved = item
        .get("ArtistItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("Name").and_then(Value::as_str))
        .filter_map(clean)
        .collect::<Vec<_>>();
    if !names.is_empty() && names != resolved {
        item.insert(
            "ArtistItems".to_string(),
            Value::Array(names.into_iter().map(|name| json!({"Name":name})).collect()),
        );
    }
}

#[cfg(test)]
mod tests {

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn available_jellyfin_track_metadata_reads_the_exact_provider_item() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/track/MetadataEditor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ExternalIdInfos": [{"Key":"MusicBrainzRecording"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Items/track"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Name":"Provider title","Etag":"revision","ArtistItems":[{"Name":"Artist","Id":"artist"}],
                "Album":"Album","ProviderIds":{"MusicBrainzRecording":"recording"}
            })))
            .mount(&server)
            .await;
        let source = JellyfinSource::open(
            super::super::JellyfinSourceConfig {
                base_url: server.uri(),
                server_id: None,
                user_id: "user".to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "token".to_string(),
            "device".to_string(),
        )
        .expect("open Jellyfin");
        let mut row = track_row();
        row.musicbrainz_release_track_id = Some("rufin-release-track".to_string());
        let metadata = source
            .read_track_metadata(row)
            .await
            .expect("read Track metadata");
        assert!(metadata.writable.title);
        assert!(metadata.writable.musicbrainz_recording_id);
        assert_eq!(metadata.revision.as_deref(), Some("etag:revision"));
        assert_eq!(metadata.values.title, "Provider title");
        assert_eq!(
            metadata.values.musicbrainz_recording_id.as_deref(),
            Some("recording")
        );
        assert_eq!(metadata.source_values.musicbrainz_release_track_id, None);
        assert_eq!(
            metadata.values.musicbrainz_release_track_id.as_deref(),
            Some("rufin-release-track")
        );
        assert!(metadata.rufin_filled.musicbrainz_release_track_id);
    }

    #[test]
    fn jellyfin_track_write_preserves_structured_artist_ids_when_names_do_not_change() {
        let original = json!([{"Name":"Artist","Id":"artist-id"}]);
        let mut item = json!({"Artists":["Artist"],"ArtistItems":original.clone()})
            .as_object()
            .cloned()
            .unwrap();
        preserve_complete_artist_items(&mut item);
        apply_track_edit(
            &mut item,
            &crate::TrackMetadataEdit {
                values: TrackMetadataValues {
                    title: "Track".to_string(),
                    artist: Some("Artist".to_string()),
                    ..TrackMetadataValues::default()
                },
                changed: crate::TrackMetadataWritable {
                    title: true,
                    artist: true,
                    ..crate::TrackMetadataWritable::default()
                },
            },
        );
        assert_eq!(item["ArtistItems"], original);
    }

    #[test]
    fn track_identification_prefers_the_exact_recording_identity() {
        let previous = TrackMetadataValues {
            title: "Current title".to_string(),
            musicbrainz_recording_id: Some("recording".to_string()),
            ..TrackMetadataValues::default()
        };
        let results = vec![
            json!({"Name":"Wrong", "ProviderIds":{"MusicBrainzRecording":"other"}}),
            json!({
                "Name":"Identified title",
                "Artists":["Identified artist"],
                "Album":"Identified album",
                "ProviderIds":{"MusicBrainzRecording":"recording"}
            }),
        ];

        let (identified, _) =
            select_track_identification(&previous, &results).expect("exact identification");
        assert_eq!(identified.title, "Identified title");
        assert_eq!(identified.artist.as_deref(), Some("Identified artist"));
        assert_eq!(identified.album.as_deref(), Some("Identified album"));
    }

    fn track_row() -> library::TrackRow {
        library::TrackRow {
            source_id: "source".to_string(),
            track_key: library::TrackKey::from_raw(1),
            source_key: library::SourceKey::from_raw(1),
            object_id: "jellyfin:track:track".to_string(),
            album_key: None,
            title: "Fallback".to_string(),
            display_album: "Album".to_string(),
            display_artist: "Artist".to_string(),
            duration_millis: 180_000,
            disc_number: 1,
            track_number: 1,
            year: None,
            release_date: None,
            date_added: None,
            media_uri: None,
            source_format: None,
            comment: None,
            bpm: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            cue_path: None,
            cue_start_millis: None,
            cue_end_millis: None,
            loudness_analysis_key: [0; 32],
            artwork_binding: None,
            favorite: false,
            rating: None,
            last_played: None,
            play_count: 0,
            skip_count: 0,
            is_downloaded: false,
            artists: Vec::new(),
            album_artists: Vec::new(),
            genres: Vec::new(),
        }
    }
}
