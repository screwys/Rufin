//! Jellyfin metadata reads, writes, and remote identification for exact provider objects.

use serde_json::{Map, Value, json};

use super::client::endpoint;
use super::{JellyfinEmbySource, ServerKind, SourceError, SourceResult};
use crate::{MetadataChanges, MetadataDraft, MetadataField, SourceEntityKind, SourceMetadataError};

const ITEM_FIELDS: &str = "Genres,ProviderIds,AlbumArtists,ArtistItems,Overview,ProductionYear,Settings,OriginalTitle,CustomRating,Tags,Studios,Etag";

impl JellyfinEmbySource {
    pub async fn write_artwork(
        &self,
        raw: &str,
        edit: &crate::ArtworkEdit,
    ) -> Result<(), SourceMetadataError> {
        if edit.storage != crate::ArtworkStorage::Server {
            return Err(SourceMetadataError::Unavailable);
        }
        self.change_artwork(raw, &edit.change)
            .await
            .map_err(metadata_write)
    }

    pub async fn change_artwork(
        &self,
        object: &str,
        change: &crate::ArtworkChange,
    ) -> SourceResult<()> {
        use base64::Engine;
        let raw = crate::policy::raw_item_id(object);
        let url = endpoint(&self.base_url, &format!("Items/{raw}/Images/Primary"))?;
        let request = match change {
            crate::ArtworkChange::Replace(image) => self
                .client
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    image
                        .content_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                )
                .body(base64::engine::general_purpose::STANDARD.encode(&image.bytes)),
            crate::ArtworkChange::Remove => self.client.delete(url),
        };
        self.send_unit(request).await
    }
    pub async fn read_track_metadata(
        &self,
        track: library::TrackView,
    ) -> Result<MetadataDraft, SourceMetadataError> {
        self.read_metadata(
            track
                .object_id
                .as_deref()
                .ok_or(SourceMetadataError::Unavailable)?,
            SourceEntityKind::Track,
            [
                ("title", Some(track.metadata.title)),
                ("album", Some(track.metadata.album)),
                (
                    "bpm",
                    track
                        .metadata
                        .bpm
                        .and_then(|v| u16::try_from(v).ok())
                        .map(|v| v.to_string()),
                ),
                (
                    "musicbrainz_recording_id",
                    track.metadata.musicbrainz_recording_id,
                ),
                (
                    "musicbrainz_release_track_id",
                    track.metadata.musicbrainz_release_track_id,
                ),
            ],
            1,
        )
        .await
    }

    pub async fn read_album_metadata(
        &self,
        album: library::AlbumView,
    ) -> Result<MetadataDraft, SourceMetadataError> {
        self.read_metadata(
            &album.object_id,
            SourceEntityKind::Album,
            [
                ("title", Some(album.metadata.title)),
                ("artist", Some(album.metadata.artist.clone())),
                ("album_artist", Some(album.metadata.artist)),
                (
                    "musicbrainz_album_id",
                    album.metadata.musicbrainz_release_id,
                ),
                (
                    "musicbrainz_release_group_id",
                    album.metadata.musicbrainz_release_group_id,
                ),
            ],
            album.summary.track_count.max(0) as usize,
        )
        .await
    }

    pub async fn read_artist_metadata(
        &self,
        artist: library::ArtistView,
    ) -> Result<MetadataDraft, SourceMetadataError> {
        self.read_metadata(
            &artist.object_id,
            SourceEntityKind::Artist,
            [
                ("title", Some(artist.metadata.name)),
                (
                    "musicbrainz_artist_id",
                    artist.metadata.musicbrainz_artist_id,
                ),
            ],
            artist.summary.track_count.max(0) as usize,
        )
        .await
    }

    async fn read_metadata<const N: usize>(
        &self,
        object: &str,
        kind: SourceEntityKind,
        fallback: [(&str, Option<String>); N],
        count: usize,
    ) -> Result<MetadataDraft, SourceMetadataError> {
        let raw = raw_id(self.kind, object, kind.as_str())?;
        let (item, editor) = self.read_item_and_editor(raw).await?;
        let scope = match kind {
            SourceEntityKind::Track => 1,
            SourceEntityKind::Album => 2,
            SourceEntityKind::Artist => 4,
        };
        let mut fields = FIELDS
            .iter()
            .filter(|(_, _, scopes)| scopes & scope != 0)
            .map(|&(key, native, _)| {
                let writable = match key {
                    "bpm" => false,
                    "musicbrainz_artist_id" if kind == SourceEntityKind::Track => false,
                    _ if key.starts_with("musicbrainz_") => external(&editor, native),
                    _ => true,
                };
                let mut field = MetadataField::new(
                    key,
                    field_value(self.kind, &item, key, native).unwrap_or_default(),
                    writable,
                );
                if field.value.is_empty()
                    && let Some((_, Some(value))) = fallback.iter().find(|(name, _)| *name == key)
                {
                    if key.starts_with("musicbrainz_") {
                        field.suggestion = Some(value.clone());
                    } else {
                        field.value = value.clone();
                    }
                }
                field
            })
            .collect::<Vec<_>>();
        fields.extend(extra_fields(self.kind, &item, &editor, kind));
        let mut draft = MetadataDraft::new(kind, fields);
        draft.revision = Some(revision(&item)?);
        draft.source_search = kind != SourceEntityKind::Track;
        draft.track_count = count;
        Ok(draft)
    }

    async fn read_item_and_editor(&self, raw: &str) -> Result<(Value, Value), SourceMetadataError> {
        let editor = endpoint(&self.base_url, &format!("Items/{raw}/MetadataEditor"))
            .map_err(metadata_write)?;
        let editor = self
            .get_json::<Value>(editor)
            .await
            .map_err(metadata_write)?;
        let item = self.read_metadata_item(raw).await.map_err(metadata_write)?;
        Ok((item, editor))
    }

    async fn read_metadata_item(&self, raw: &str) -> SourceResult<Value> {
        let mut url = self.item_url(raw)?;
        url.query_pairs_mut().append_pair("Fields", ITEM_FIELDS);
        self.get_json(url).await
    }

    pub async fn write_metadata(
        &self,
        object_id: &str,
        expected_revision: &str,
        application: Option<&str>,
        edit: &crate::MetadataEdit,
    ) -> Result<String, crate::SourceMetadataError> {
        self.write_metadata_value(
            object_id,
            edit.kind.as_str(),
            expected_revision,
            application,
            !edit.changes.is_empty(),
            |item| apply_edit(self.kind, item, edit),
        )
        .await
    }

    pub async fn write_metadata_value(
        &self,
        object_id: &str,
        kind: &str,
        expected_revision: &str,
        application: Option<&str>,
        tags_changed: bool,
        update: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<String, SourceMetadataError> {
        let raw = raw_id(self.kind, object_id, kind)?;
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
        if !tags_changed {
            return Ok(raw.to_string());
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

    pub async fn identify_album_metadata(
        &self,
        object_id: &str,
        values: &MetadataChanges,
    ) -> Result<Option<(MetadataChanges, String)>, String> {
        let raw = raw_id(self.kind, object_id, "album").map_err(|error| error.to_string())?;
        let mut search_info = json!({
            "Name": values.get("title"),
            "Year": values.get("year").and_then(|value| value.parse::<u16>().ok()),
            "ProviderIds": identification_provider_ids([
                ("MusicBrainzAlbum", values.get("musicbrainz_album_id").map(String::as_str)),
                (
                    "MusicBrainzReleaseGroup",
                    values.get("musicbrainz_release_group_id").map(String::as_str),
                ),
            ]),
        });
        let album_artists = split_values(
            values
                .get("album_artist")
                .filter(|value| !value.trim().is_empty())
                .or_else(|| values.get("artist"))
                .map(String::as_str),
        );
        if !album_artists.is_empty() {
            search_info
                .as_object_mut()
                .expect("Jellyfin search info object")
                .insert("AlbumArtists".to_string(), json!(album_artists));
        }
        let results = self.remote_search(raw, "MusicAlbum", search_info).await?;
        Ok(select_identification(
            values,
            &results,
            SourceEntityKind::Album,
        ))
    }

    pub async fn identify_artist_metadata(
        &self,
        object_id: &str,
        values: &MetadataChanges,
    ) -> Result<Option<(MetadataChanges, String)>, String> {
        let raw = raw_id(self.kind, object_id, "artist").map_err(|error| error.to_string())?;
        let search_info = json!({
            "Name": values.get("title"),
            "ProviderIds": identification_provider_ids([
                ("MusicBrainzArtist", values.get("musicbrainz_artist_id").map(String::as_str)),
            ]),
        });
        let results = self.remote_search(raw, "MusicArtist", search_info).await?;
        Ok(select_identification(
            values,
            &results,
            SourceEntityKind::Artist,
        ))
    }

    async fn remote_search(
        &self,
        raw: &str,
        item_type: &str,
        search_info: Value,
    ) -> Result<Vec<Value>, String> {
        if search_info
            .get("Name")
            .and_then(Value::as_str)
            .is_none_or(|name| name.trim().is_empty())
            && search_info
                .get("ProviderIds")
                .and_then(Value::as_object)
                .is_none_or(Map::is_empty)
        {
            return Ok(Vec::new());
        }
        let url = endpoint(&self.base_url, &format!("Items/RemoteSearch/{item_type}"))
            .map_err(|error| error.to_string())?;
        self.send_json(self.client.post(url).json(&json!({
            "ItemId": raw,
            "SearchInfo": search_info,
        })))
        .await
        .map_err(|error| error.to_string())
    }
}

fn identification_provider_ids<'a>(
    values: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Map<String, Value> {
    values
        .into_iter()
        .filter_map(|(key, value)| Some((key.to_string(), Value::String(clean(value?)?))))
        .collect()
}

fn split_values(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(';'))
        .filter_map(clean)
        .collect()
}

// Track, album and artist scopes share the same field meanings and native keys.
const FIELDS: &[(&str, &str, u8)] = &[
    ("title", "Name", 7),
    ("sort_title", "ForcedSortName", 7),
    ("artist", "ArtistItems", 3),
    ("album", "Album", 1),
    ("album_artist", "AlbumArtists", 3),
    ("track_number", "IndexNumber", 1),
    ("disc_number", "ParentIndexNumber", 1),
    ("year", "ProductionYear", 3),
    ("genre", "Genres", 7),
    ("comment", "Overview", 7),
    ("bpm", "", 1),
    ("locked", "LockData", 7),
    ("musicbrainz_recording_id", "MusicBrainzRecording", 1),
    ("musicbrainz_release_track_id", "MusicBrainzTrack", 1),
    ("musicbrainz_album_id", "MusicBrainzAlbum", 3),
    ("musicbrainz_release_group_id", "MusicBrainzReleaseGroup", 3),
    ("musicbrainz_artist_id", "MusicBrainzArtist", 5),
];

fn field_value(server: ServerKind, item: &Value, key: &str, native: &str) -> Option<String> {
    match key {
        "track_number" | "disc_number" | "year" => {
            number(item, native).map(|value| value.to_string())
        }
        "locked" => boolean(item, native).map(|value| value.to_string()),
        "artist" => named(item, native).or_else(|| string_array(item, "Artists")),
        "album_artist" => named(item, native),
        "genre" if server == ServerKind::Emby => named(item, "GenreItems"),
        "genre" => string_array(item, native),
        _ if key.starts_with("musicbrainz_") => provider(item, native),
        _ => string(item, native),
    }
}

pub(crate) fn apply_edit(
    server: ServerKind,
    item: &mut Map<String, Value>,
    edit: &crate::MetadataEdit,
) {
    for (key, value) in &edit.changes {
        if let Some(provider) = key.strip_prefix("provider:") {
            set_provider(item, provider, Some(value));
            continue;
        }
        let native = FIELDS
            .iter()
            .find(|(field, _, _)| *field == key)
            .map(|(_, native, _)| *native)
            .or_else(|| {
                EXTRA_FIELDS
                    .iter()
                    .find(|(field, _, _)| *field == key)
                    .map(|(field, _, _)| *field)
            });
        let Some(native) = native else {
            continue;
        };
        match key.as_str() {
            "Tags" if server == ServerKind::Emby => set_named_list(item, "TagItems", value),
            "Tags" => {
                item.insert(key.clone(), json!(split_values(Some(value))));
            }
            "Studios" => set_named_list(item, key, value),
            "PremiereDate" if value.trim().len() == 10 => {
                set_string(item, key, Some(&format!("{}T00:00:00Z", value.trim())))
            }
            "title" => set_required(item, native, value),
            "artist" | "album_artist" => set_named_if_changed(item, native, Some(value)),
            "genre" => set_genres(server, item, Some(value)),
            "track_number" | "disc_number" | "year" => set_number(item, native, value.parse().ok()),
            "locked" => {
                if let Ok(value) = value.parse::<bool>() {
                    item.insert(native.into(), Value::Bool(value));
                }
            }
            "bpm" => {}
            key if key.starts_with("musicbrainz_") => set_provider(item, native, Some(value)),
            _ => set_string(item, native, Some(value)),
        }
    }
}

const EXTRA_FIELDS: &[(&str, &str, crate::MetadataFieldKind)] = &[
    (
        "PremiereDate",
        "Release date",
        crate::MetadataFieldKind::Date,
    ),
    ("Tags", "Tags", crate::MetadataFieldKind::List),
    ("Studios", "Studios", crate::MetadataFieldKind::List),
];

fn extra_fields(
    server: ServerKind,
    item: &Value,
    editor: &Value,
    kind: SourceEntityKind,
) -> Vec<crate::MetadataField> {
    let mut fields = EXTRA_FIELDS
        .iter()
        .map(|&(key, label, kind)| {
            let value = match key {
                "Tags" if server == ServerKind::Emby => named(item, "TagItems"),
                "Tags" => string_array(item, key),
                "Studios" => named(item, key),
                "PremiereDate" => string(item, key)
                    .map(|value| value.split('T').next().unwrap_or(&value).to_string()),
                _ => string(item, key),
            }
            .unwrap_or_default();
            crate::MetadataField {
                key: key.into(),
                label: label.into(),
                kind,
                value,
                writable: true,
                mixed: false,
                suggestion: None,
            }
        })
        .collect::<Vec<_>>();
    let scope = match kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    };
    for info in crate::remote_json::items(&editor["ExternalIdInfos"]) {
        let Some(key) = info["Key"].as_str() else {
            continue;
        };
        if FIELDS
            .iter()
            .any(|(_, native, scopes)| scopes & scope != 0 && key.eq_ignore_ascii_case(native))
        {
            continue;
        }
        let label = match key {
            "MusicBrainzArtist" => "MusicBrainz artist ID".into(),
            "MusicBrainzAlbumArtist" => "MusicBrainz album artist ID".into(),
            "AudioDbAlbum" => "TheAudioDB album ID".into(),
            "AudioDbArtist" => "TheAudioDB artist ID".into(),
            _ => {
                let name = info["Name"].as_str().unwrap_or(key);
                match info["Type"].as_str().filter(|kind| !kind.is_empty()) {
                    Some(kind) => format!("{name} ({kind})"),
                    None => name.into(),
                }
            }
        };
        fields.push(crate::MetadataField {
            key: format!("provider:{key}"),
            label,
            kind: crate::MetadataFieldKind::Text,
            value: provider(item, key).unwrap_or_default(),
            writable: true,
            mixed: false,
            suggestion: None,
        });
    }
    fields
}

fn set_named_list(item: &mut Map<String, Value>, key: &str, value: &str) {
    let values = split_values(Some(value))
        .into_iter()
        .map(|name| {
            item.get(key)
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item["Name"].as_str() == Some(&name))
                })
                .cloned()
                .unwrap_or_else(|| json!({"Name": name}))
        })
        .collect();
    item.insert(key.into(), Value::Array(values));
}

fn external(info: &Value, key: &str) -> bool {
    crate::remote_json::items(&info["ExternalIdInfos"])
        .iter()
        .any(|value| {
            value["Key"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(key))
        })
}

fn select_identification(
    previous: &MetadataChanges,
    results: &[Value],
    kind: SourceEntityKind,
) -> Option<(MetadataChanges, String)> {
    let keys: &[&str] = if kind == SourceEntityKind::Album {
        &[
            "title",
            "year",
            "artist",
            "album_artist",
            "musicbrainz_album_id",
            "musicbrainz_release_group_id",
        ]
    } else {
        &["title", "musicbrainz_artist_id"]
    };
    let fields = FIELDS
        .iter()
        .filter(|(key, _, _)| keys.contains(key))
        .collect::<Vec<_>>();
    let exact = results
        .iter()
        .filter(|item| {
            fields.iter().any(|(key, native, _)| {
                key.starts_with("musicbrainz_")
                    && previous
                        .get(*key)
                        .is_some_and(|id| provider(item, native).as_ref() == Some(id))
            })
        })
        .collect::<Vec<_>>();
    let selected = match exact.as_slice() {
        [one] => *one,
        [] => select_result(
            previous.get("title").map_or("", String::as_str),
            previous.get("year").and_then(|v| v.parse().ok()),
            results,
        )?,
        _ => return None,
    };
    let changes = fields
        .into_iter()
        .filter_map(|(key, native, _)| {
            field_value(ServerKind::Jellyfin, selected, key, native)
                .map(|value| ((*key).into(), value))
        })
        .collect();
    Some((changes, serde_json::to_string(selected).ok()?))
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

fn raw_id<'a>(
    server: ServerKind,
    object_id: &'a str,
    kind: &str,
) -> Result<&'a str, SourceMetadataError> {
    object_id
        .strip_prefix(&server.object_id(kind, ""))
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
fn set_genres(server: ServerKind, item: &mut Map<String, Value>, value: Option<&str>) {
    if server == ServerKind::Jellyfin {
        return set_strings(item, "Genres", value);
    }
    let genres = split(value)
        .into_iter()
        .map(|name| {
            item.get("GenreItems")
                .and_then(Value::as_array)
                .and_then(|genres| {
                    genres
                        .iter()
                        .find(|genre| genre["Name"].as_str() == Some(&name))
                })
                .cloned()
                .unwrap_or_else(|| json!({"Name": name}))
        })
        .collect();
    item.insert("GenreItems".into(), Value::Array(genres));
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

    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn unavailable_external_ids_leave_ordinary_metadata_writable() {
        for info in [
            json!({}),
            json!({"ExternalIdInfos":null}),
            json!({"ExternalIdInfos":{}}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/Items/item/MetadataEditor"))
                .respond_with(ResponseTemplate::new(200).set_body_json(info))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/Items/item"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"Name":"Item","Etag":"revision"})),
                )
                .mount(&server)
                .await;
            let source = test_source(&server);
            for kind in [
                SourceEntityKind::Track,
                SourceEntityKind::Album,
                SourceEntityKind::Artist,
            ] {
                let draft = source
                    .read_metadata(&format!("jellyfin:{}:item", kind.as_str()), kind, [], 1)
                    .await
                    .unwrap();
                assert!(
                    draft
                        .fields
                        .iter()
                        .any(|field| field.key == "title" && field.writable)
                );
                assert!(
                    draft
                        .fields
                        .iter()
                        .filter(|field| field.key.starts_with("musicbrainz_"))
                        .all(|field| !field.writable)
                );
            }
        }
    }

    #[test]
    fn emby_genre_edits_preserve_identifiers_and_unedited_metadata() {
        let mut item = json!({"Id":"11","Name":"Song","GenreItems":[{"Id":"1","Name":"Rock"},{"Id":"2","Name":"Pop"}],"ArtistItems":[{"Id":"artist","Name":"Artist"}],"ProviderIds":{"MusicBrainzTrack":"release-track"}}).as_object().unwrap().clone();
        let original = item.clone();
        let mut edit = crate::MetadataEdit {
            kind: SourceEntityKind::Track,
            artwork: None,
            changes: [("genre".into(), "Rock; Jazz".into())].into(),
        };
        apply_edit(ServerKind::Emby, &mut item, &edit);
        assert_eq!(
            item["GenreItems"],
            json!([{"Id":"1","Name":"Rock"},{"Name":"Jazz"}])
        );
        for field in ["Id", "Name", "ArtistItems", "ProviderIds"] {
            assert_eq!(item[field], original[field]);
        }
        edit.changes.insert("genre".into(), String::new());
        apply_edit(ServerKind::Emby, &mut item, &edit);
        assert_eq!(item["GenreItems"], json!([]));
        assert!(!item.contains_key("Genres"));
        assert!(raw_id(ServerKind::Emby, "emby:track:11", "track").is_ok());
        assert!(raw_id(ServerKind::Emby, "jellyfin:track:11", "track").is_err());
    }

    #[tokio::test]
    async fn available_jellyfin_track_metadata_reads_the_exact_provider_item() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/track/MetadataEditor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ExternalIdInfos": [null, {"Key":{}}, {"Key":"MusicBrainzRecording"}]
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
        let source = test_source(&server);
        let mut row = track_row();
        row.metadata.musicbrainz_release_track_id = Some("rufin-release-track".to_string());
        let metadata = source
            .read_track_metadata(row)
            .await
            .expect("read Track metadata");
        let field = |key| {
            metadata
                .fields
                .iter()
                .find(|field| field.key == key)
                .unwrap()
        };
        assert!(field("title").writable);
        assert!(field("musicbrainz_recording_id").writable);
        assert!(!metadata.source_search);
        assert_eq!(metadata.revision.as_deref(), Some("etag:revision"));
        assert_eq!(field("title").value, "Provider title");
        assert_eq!(field("musicbrainz_recording_id").value, "recording");
        assert!(field("musicbrainz_release_track_id").value.is_empty());
        assert_eq!(
            field("musicbrainz_release_track_id").suggestion.as_deref(),
            Some("rufin-release-track")
        );
    }

    #[tokio::test]
    async fn jellyfin_album_identification_keeps_exact_ids_and_artist_credits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Items/RemoteSearch/MusicAlbum"))
            .and(body_json(json!({
                "ItemId": "album",
                "SearchInfo": {
                    "Name": "Album",
                    "Year": 2024,
                    "ProviderIds": {
                        "MusicBrainzAlbum": "release",
                        "MusicBrainzReleaseGroup": "release-group",
                    },
                    "AlbumArtists": ["Artist", "Guest"],
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        let source = test_source(&server);

        for artist_field in ["album_artist", "artist"] {
            let mut values: MetadataChanges = [
                ("title".into(), "Album".into()),
                ("album_artist".into(), String::new()),
                ("year".into(), "2024".into()),
                ("musicbrainz_album_id".into(), "release".into()),
                (
                    "musicbrainz_release_group_id".into(),
                    "release-group".into(),
                ),
            ]
            .into();
            values.insert(artist_field.into(), "Artist; Guest".into());
            let result = source
                .identify_album_metadata("jellyfin:album:album", &values)
                .await
                .expect("identify Jellyfin Album");
            assert!(result.is_none());
        }
    }

    #[test]
    fn jellyfin_track_write_preserves_structured_artist_ids_when_names_do_not_change() {
        let original = json!([{"Name":"Artist","Id":"artist-id"}]);
        let mut item = json!({"Artists":["Artist"],"ArtistItems":original.clone()})
            .as_object()
            .cloned()
            .unwrap();
        preserve_complete_artist_items(&mut item);
        apply_edit(
            crate::ServerKind::Jellyfin,
            &mut item,
            &crate::MetadataEdit {
                kind: SourceEntityKind::Track,
                artwork: None,
                changes: [
                    ("title".into(), "Track".into()),
                    ("artist".into(), "Artist".into()),
                ]
                .into(),
            },
        );
        assert_eq!(item["ArtistItems"], original);
    }

    fn track_row() -> library::TrackView {
        library::TrackView {
            track_key: library::TrackKey::from_raw(1),
            source_key: Some(library::SourceKey::from_raw(1)),
            source_id: Some("source".into()),
            object_id: Some("jellyfin:track:track".into()),
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "track",
                "jellyfin:track:track",
            ),
            metadata: library::TrackMetadata {
                title: "Fallback".into(),
                album: "Album".into(),
                artist: "Artist".into(),
                duration_millis: 180_000,
                disc_number: 1,
                track_number: 1,
                ..Default::default()
            },
            access: Default::default(),
            state: Default::default(),
            links: Default::default(),
            artwork_binding: None,
        }
    }

    fn test_source(server: &MockServer) -> JellyfinEmbySource {
        JellyfinEmbySource::open(
            super::super::JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: crate::ServerKind::Jellyfin,
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
        .expect("open Jellyfin")
    }
}
