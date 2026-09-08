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
    let names: Vec<_> = items(&item["Genre"])
        .iter()
        .filter_map(|genre| genre["tag"].as_str())
        .collect();
    (!names.is_empty()).then(|| names.join("; "))
}
fn revision(item: &Value) -> String {
    id(&item["updatedAt"]).unwrap_or_default()
}
impl PlexSource {
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
        let item = self.metadata(object).await.map_err(error)?;
        if revision(&item) != expected {
            return Err(Error::Conflict);
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
            if key == "genre[0].tag" {
                params.push(("genre.locked".into(), "1".into()));
                params.push((
                    "genre[].tag.tag-".into(),
                    genre(&item).unwrap_or_default().replace("; ", ","),
                ));
                for (index, tag) in value
                    .split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .enumerate()
                {
                    params.push((format!("genre[{index}].tag.tag"), tag.into()));
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
