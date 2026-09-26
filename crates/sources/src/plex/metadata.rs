use super::*;
use crate::remote_json::{field, id, items};
use crate::{
    AlbumMetadataValues, ArtistMetadataValues, SourceMetadataError as Error, TrackMetadataValues,
};
fn error(error: SourceError) -> Error {
    Error::Write(error.to_string())
}
fn text(item: &Value, key: &str) -> Option<String> {
    item[key].as_str().map(str::to_string)
}
fn genre(item: &Value) -> Option<String> {
    tags(item, "Genre")
}
fn tags(item: &Value, key: &str) -> Option<String> {
    let names: Vec<_> = items(&item[key])
        .iter()
        .filter_map(|genre| genre["tag"].as_str())
        .collect();
    (!names.is_empty()).then(|| names.join("; "))
}
fn revision(item: &Value) -> String {
    id(&item["updatedAt"]).unwrap_or_default()
}

const EXTRA_FIELDS: &[(&str, &str, &str, crate::MetadataFieldKind, u8)] = &[
    (
        "mood[0].tag",
        "Mood",
        "Mood",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "collection[0].tag",
        "Collections",
        "Collection",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "label[0].tag",
        "Labels",
        "Label",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "originallyAvailableAt",
        "Release date",
        "originallyAvailableAt",
        crate::MetadataFieldKind::Date,
        2,
    ),
    (
        "studio",
        "Studio",
        "studio",
        crate::MetadataFieldKind::Text,
        2,
    ),
    (
        "style[0].tag",
        "Style",
        "Style",
        crate::MetadataFieldKind::List,
        6,
    ),
    (
        "country[0].tag",
        "Country",
        "Country",
        crate::MetadataFieldKind::List,
        4,
    ),
    (
        "similar[0].tag",
        "Similar artists",
        "Similar",
        crate::MetadataFieldKind::List,
        4,
    ),
];

fn extra_fields(item: &Value, writable: bool, scope: u8) -> Vec<crate::MetadataField> {
    EXTRA_FIELDS
        .iter()
        .filter(|(_, _, _, _, scopes)| scopes & scope != 0)
        .map(|&(key, label, json, kind, _)| crate::MetadataField {
            key: key.into(),
            label: label.into(),
            kind,
            value: if kind == crate::MetadataFieldKind::List {
                tags(item, json)
            } else {
                text(item, json)
            }
            .unwrap_or_default(),
            writable,
            mixed: false,
        })
        .collect()
}

fn extra_edits(changes: &crate::MetadataChanges, fields: &mut Vec<(&'static str, String)>) {
    for (key, value) in changes {
        if let Some((key, _, _, _, _)) =
            EXTRA_FIELDS.iter().find(|(field, _, _, _, _)| field == key)
        {
            fields.push((key, value.clone()));
        }
    }
}
impl PlexSource {
    pub(crate) async fn write_artwork(
        &self,
        object: &str,
        edit: Option<&crate::ArtworkEdit>,
    ) -> Result<(), Error> {
        let Some(edit) = edit else { return Ok(()) };
        if edit.storage != crate::ArtworkStorage::Server || !self.config.owned {
            return Err(Error::Unavailable);
        }
        self.change_artwork(object, &edit.change)
            .await
            .map_err(error)
    }

    pub(crate) async fn change_artwork(
        &self,
        object: &str,
        change: &crate::ArtworkChange,
    ) -> super::SourceResult<()> {
        let (method, endpoint) = match change {
            crate::ArtworkChange::Replace(_) => (Method::POST, "posters"),
            crate::ArtworkChange::Remove => (Method::DELETE, "thumb"),
        };
        let path = format!(
            "/library/metadata/{}/{endpoint}",
            crate::policy::raw_item_id(object)
        );
        let mut request = self.request(method, &path, &[]).await?;
        if let crate::ArtworkChange::Replace(image) = change {
            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    image
                        .content_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                )
                .body(image.bytes.clone());
        }
        crate::remote_http::unit(request, HTTP).await
    }
    pub(crate) async fn read_track_metadata(
        &self,
        track: library::TrackRow,
    ) -> Result<crate::TrackMetadata, Error> {
        let item = self.metadata(&track.object_id).await.map_err(error)?;
        let values = TrackMetadataValues {
            title: text(&item, "title").unwrap_or(track.title),
            sort_title: text(&item, "titleSort"),
            artist: text(&item, "originalTitle").or_else(|| text(&item, "grandparentTitle")),
            album: text(&item, "parentTitle"),
            album_artist: text(&item, "grandparentTitle"),
            track_number: field(&item, "index"),
            disc_number: field(&item, "parentIndex"),
            year: field(&item, "year")
                .filter(|year| *year > 0)
                .or_else(|| field(&item, "parentYear").filter(|year| *year > 0)),
            genre: genre(&item),
            comment: text(&item, "summary"),
            ..Default::default()
        };
        let yes = self.config.owned;
        Ok(crate::TrackMetadata {
            extra: extra_fields(&item, yes, 1),
            artwork: Default::default(),
            writable: crate::TrackMetadataWritable {
                title: yes,
                sort_title: yes,
                artist: yes,
                track_number: yes,
                disc_number: yes,
                genre: yes,
                comment: yes,
                ..Default::default()
            },
            source_search: false,
            revision: Some(revision(&item)),
            source_values: values.clone(),
            values,
            rufin_filled: Default::default(),
        })
    }
    pub(crate) async fn read_album_metadata(
        &self,
        album: library::AlbumRow,
    ) -> Result<crate::AlbumMetadata, Error> {
        let item = self.metadata(&album.object_id).await.map_err(error)?;
        let values = AlbumMetadataValues {
            title: text(&item, "title").unwrap_or(album.title),
            sort_title: text(&item, "titleSort"),
            album_artist: text(&item, "parentTitle"),
            year: field(&item, "year"),
            genre: genre(&item),
            comment: text(&item, "summary"),
            ..Default::default()
        };
        let yes = self.config.owned;
        Ok(crate::AlbumMetadata {
            extra: extra_fields(&item, yes, 2),
            artwork: Default::default(),
            writable: crate::AlbumMetadataWritable {
                title: yes,
                sort_title: yes,
                year: yes,
                genre: yes,
                comment: yes,
                ..Default::default()
            },
            source_search: false,
            revision: Some(revision(&item)),
            source_values: values.clone(),
            values,
            rufin_filled: Default::default(),
            track_count: album.track_count.max(0) as usize,
            mixed: Default::default(),
        })
    }
    pub(crate) async fn read_artist_metadata(
        &self,
        artist: library::ArtistRow,
    ) -> Result<crate::ArtistMetadata, Error> {
        let item = self.metadata(&artist.object_id).await.map_err(error)?;
        let values = ArtistMetadataValues {
            name: text(&item, "title").unwrap_or(artist.name),
            sort_name: text(&item, "titleSort"),
            genre: genre(&item),
            comment: text(&item, "summary"),
            ..Default::default()
        };
        let yes = self.config.owned;
        Ok(crate::ArtistMetadata {
            extra: extra_fields(&item, yes, 4),
            artwork: Default::default(),
            writable: crate::ArtistMetadataWritable {
                name: yes,
                sort_name: yes,
                genre: yes,
                comment: yes,
                ..Default::default()
            },
            source_search: false,
            revision: Some(revision(&item)),
            source_values: values.clone(),
            values,
            rufin_filled: Default::default(),
            track_count: artist.track_count.max(0) as usize,
            mixed: Default::default(),
        })
    }
    async fn write_fields(
        &self,
        object: &str,
        expected: &str,
        kind: &str,
        fields: Vec<(&str, String)>,
    ) -> Result<(), Error> {
        if !self.config.owned {
            return Err(Error::Unavailable);
        }
        let item = self.metadata(object).await.map_err(error)?;
        if revision(&item) != expected {
            return Err(Error::Conflict);
        }
        if fields.is_empty() {
            return Ok(());
        }
        let section = id(&item["librarySectionID"]).ok_or(Error::Unavailable)?;
        let mut params = vec![
            (
                "id".to_string(),
                crate::policy::raw_item_id(object).to_string(),
            ),
            ("type".to_string(), kind.to_string()),
        ];
        if kind == "9" && fields.iter().any(|(key, _)| *key == "title") {
            if let Some(artist) = id(&item["parentRatingKey"]) {
                params.push(("artist.id.value".into(), artist));
            }
        }
        for (key, value) in fields {
            if let Some(name) = key.strip_suffix("[0].tag") {
                let json_key = EXTRA_FIELDS
                    .iter()
                    .find(|(field, _, _, _, _)| *field == key)
                    .map_or("Genre", |(_, _, json, _, _)| *json);
                params.push((format!("{name}.locked"), "1".into()));
                params.push((
                    format!("{name}[].tag.tag-"),
                    tags(&item, json_key).unwrap_or_default().replace("; ", ","),
                ));
                for (index, tag) in value
                    .split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .enumerate()
                {
                    params.push((format!("{name}[{index}].tag.tag"), tag.into()));
                }
            } else {
                params.push((format!("{key}.value"), value));
                params.push((format!("{key}.locked"), "1".into()));
            }
        }
        let borrowed: Vec<_> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        self.unit(
            Method::PUT,
            &format!("/library/sections/{section}/all"),
            &borrowed,
        )
        .await
        .map_err(error)
    }
    pub(crate) async fn write_track_metadata(
        &self,
        object: &str,
        expected: &str,
        edit: &crate::TrackMetadataEdit,
    ) -> Result<(), Error> {
        let mut fields = Vec::new();
        let v = &edit.values;
        let c = &edit.changed;
        if c.title {
            fields.push(("title", v.title.clone()));
        }
        if c.sort_title {
            fields.push(("titleSort", v.sort_title.clone().unwrap_or_default()));
        }
        if c.artist {
            fields.push(("originalTitle", v.artist.clone().unwrap_or_default()));
        }
        if c.track_number {
            fields.push((
                "index",
                v.track_number.map(|v| v.to_string()).unwrap_or_default(),
            ));
        }
        if c.disc_number {
            fields.push((
                "parentIndex",
                v.disc_number.map(|v| v.to_string()).unwrap_or_default(),
            ));
        }
        if c.comment {
            fields.push(("summary", v.comment.clone().unwrap_or_default()));
        }
        if c.genre {
            fields.push(("genre[0].tag", v.genre.clone().unwrap_or_default()));
        }
        extra_edits(&edit.extra, &mut fields);
        self.write_fields(object, expected, "10", fields).await
    }
    pub(crate) async fn write_album_metadata(
        &self,
        object: &str,
        expected: &str,
        edit: &crate::AlbumMetadataEdit,
    ) -> Result<(), Error> {
        let mut fields = Vec::new();
        let v = &edit.values;
        let c = &edit.changed;
        if c.title {
            fields.push(("title", v.title.clone()));
        }
        if c.sort_title {
            fields.push(("titleSort", v.sort_title.clone().unwrap_or_default()));
        }
        if c.year {
            fields.push(("year", v.year.map(|v| v.to_string()).unwrap_or_default()));
        }
        if c.comment {
            fields.push(("summary", v.comment.clone().unwrap_or_default()));
        }
        if c.genre {
            fields.push(("genre[0].tag", v.genre.clone().unwrap_or_default()));
        }
        extra_edits(&edit.extra, &mut fields);
        self.write_fields(object, expected, "9", fields).await
    }
    pub(crate) async fn write_artist_metadata(
        &self,
        object: &str,
        expected: &str,
        edit: &crate::ArtistMetadataEdit,
    ) -> Result<(), Error> {
        let mut fields = Vec::new();
        let v = &edit.values;
        let c = &edit.changed;
        if c.name {
            fields.push(("title", v.name.clone()));
        }
        if c.sort_name {
            fields.push(("titleSort", v.sort_name.clone().unwrap_or_default()));
        }
        if c.comment {
            fields.push(("summary", v.comment.clone().unwrap_or_default()));
        }
        if c.genre {
            fields.push(("genre[0].tag", v.genre.clone().unwrap_or_default()));
        }
        extra_edits(&edit.extra, &mut fields);
        self.write_fields(object, expected, "8", fields).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn album_title_edit_keeps_artist_identity_and_locks_only_changed_fields() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET")).and(path("/library/metadata/album")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"album","type":"album","librarySectionID":"section","updatedAt":5,"parentRatingKey":"artist","title":"Before","year":[],"Genre":[{"tag":"Rock"}]}]}}))).expect(1).mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/library/sections/section/all"))
            .and(query_param("type", "9"))
            .and(query_param("id", "album"))
            .and(query_param("title.value", "After"))
            .and(query_param("title.locked", "1"))
            .and(query_param("artist.id.value", "artist"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        source
            .write_album_metadata(
                "plex:album:album",
                "5",
                &crate::AlbumMetadataEdit {
                    extra: Default::default(),
                    artwork: None,
                    values: AlbumMetadataValues {
                        title: "After".into(),
                        ..Default::default()
                    },
                    changed: crate::AlbumMetadataWritable {
                        title: true,
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(
            !requests[1]
                .url
                .query_pairs()
                .any(|(key, _)| key.starts_with("year") || key.starts_with("genre"))
        );
    }
}
