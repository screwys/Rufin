//! The server's internet radio stations. Everyone may list them; a server lets only an admin
//! change them.
use super::*;

impl SubsonicSource {
    pub async fn stations(&self) -> SourceResult<Vec<library::StationWrite>> {
        let body = self.get_json("getInternetRadioStations", &[]).await?;
        Ok(
            json::items(&body["internetRadioStations"]["internetRadioStation"])
                .iter()
                .filter_map(station)
                .collect(),
        )
    }

    /// Creates the station and answers it as the server lists it. The server answers a create
    /// with nothing, so the new one is the listed station of this name and stream that was not
    /// there before.
    pub async fn create_station(
        &self,
        name: &str,
        stream_url: &str,
        homepage: &str,
    ) -> SourceResult<library::StationWrite> {
        let before = self.stations().await?;
        let mut extra = vec![
            ("name", name.trim().to_string()),
            ("streamUrl", stream_url.trim().to_string()),
        ];
        if !homepage.trim().is_empty() {
            extra.push(("homepageUrl", homepage.trim().to_string()));
        }
        self.change("createInternetRadioStation", &extra).await?;
        self.stations()
            .await?
            .into_iter()
            .find(|station| {
                station.stream_url == stream_url.trim()
                    && !before
                        .iter()
                        .any(|known| known.object_id == station.object_id)
            })
            .ok_or(SourceError::NotFound)
    }

    pub async fn update_station(
        &self,
        id: &str,
        name: &str,
        stream_url: &str,
        homepage: &str,
    ) -> SourceResult<()> {
        self.change(
            "updateInternetRadioStation",
            &[
                ("id", id.to_string()),
                ("name", name.trim().to_string()),
                ("streamUrl", stream_url.trim().to_string()),
                ("homepageUrl", homepage.trim().to_string()),
            ],
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_station(&self, id: &str) -> SourceResult<()> {
        self.get_unit("deleteInternetRadioStation", &[("id", id.to_string())])
            .await
    }
}

fn station(value: &serde_json::Value) -> Option<library::StationWrite> {
    let text = |key: &str| value[key].as_str().unwrap_or_default().trim().to_string();
    let station = library::StationWrite {
        object_id: json::id(&value["id"])?,
        name: text("name"),
        stream_url: text("streamUrl"),
        // The response names it homePageUrl; older servers answered homepageUrl.
        homepage: Some(text("homePageUrl"))
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| text("homepageUrl")),
        cover: Some(text("coverArt")).filter(|cover| !cover.is_empty()),
    };
    (!station.stream_url.is_empty()).then_some(station)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_station_keeps_its_id_stream_homepage_and_cover() {
        let listed = serde_json::json!({"id": "ra-1", "name": " Jazz ", "streamUrl": "https://radio.example/jazz",
            "homePageUrl": "https://radio.example", "coverArt": "ra-1_0"});
        assert_eq!(
            station(&listed),
            Some(library::StationWrite {
                object_id: "ra-1".into(),
                name: "Jazz".into(),
                stream_url: "https://radio.example/jazz".into(),
                homepage: "https://radio.example".into(),
                cover: Some("ra-1_0".into()),
            })
        );
        assert_eq!(
            station(&serde_json::json!({"id": 4, "name": "No stream"})),
            None
        );
    }
}
