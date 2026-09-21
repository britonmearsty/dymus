use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://de1.api.radio-browser.info/json";

#[derive(Clone, Debug, Default)]
pub struct StationFilter {
    pub name: String,
    pub country: String,
    pub language: String,
    pub tag: String,
    pub order: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Station {
    pub stationuuid: String,
    pub name: String,
    pub url_resolved: String,
    pub homepage: String,
    pub country: String,
    pub countrycode: String,
    pub language: String,
    pub tags: String,
    pub codec: String,
    pub bitrate: u32,
    pub clickcount: u64,
    pub lastcheckok: u8,
}

#[derive(Clone)]
pub struct RadioBrowser {
    client: Client,
}

impl RadioBrowser {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent(concat!("dymus/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    pub async fn search(&self, filter: &StationFilter) -> Result<Vec<Station>> {
        let mut request = self.client.get(format!("{API}/stations/search"));
        let name = normalized_filter_value(&filter.name);
        let country = normalized_filter_value(&filter.country);
        let language = normalized_filter_value(&filter.language);
        let tag = normalized_filter_value(&filter.tag);
        let mut params = vec![
            (
                "order",
                if filter.order.is_empty() {
                    "clickcount"
                } else {
                    &filter.order
                },
            ),
            ("reverse", "true"),
            ("hidebroken", "true"),
            ("limit", "100"),
        ];
        for (key, value) in [
            ("name", name.as_str()),
            ("country", country.as_str()),
            ("language", language.as_str()),
            ("tag", tag.as_str()),
        ] {
            if !value.is_empty() {
                params.push((key, value));
            }
        }
        request = request.query(&params);
        request
            .send()
            .await
            .context("Cannot reach Radio Browser")?
            .error_for_status()
            .context("Radio Browser rejected the station search")?
            .json::<Vec<Station>>()
            .await
            .context("Radio Browser returned invalid station data")
    }

    pub async fn count_click(&self, station_uuid: &str) {
        let _ = self
            .client
            .get(format!("{API}/url/{station_uuid}"))
            .send()
            .await;
    }
}

fn normalized_filter_value(value: &str) -> String {
    value.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_station_metadata_with_optional_fields() {
        let station: Station = serde_json::from_str(
            r#"{"stationuuid":"abc","name":"Jazz FM","url_resolved":"https://stream.test/live","country":"Kenya","language":"English","tags":"jazz","codec":"MP3","bitrate":128,"clickcount":50,"lastcheckok":1}"#,
        )
        .unwrap();
        assert_eq!(station.name, "Jazz FM");
        assert_eq!(station.country, "Kenya");
        assert_eq!(station.bitrate, 128);
        assert_eq!(station.lastcheckok, 1);
    }

    #[test]
    fn normalizes_filters_without_changing_the_search_meaning() {
        assert_eq!(normalized_filter_value("  KENYA  "), "kenya");
        assert_eq!(normalized_filter_value("Jàzz FM"), "jàzz fm");
    }
}
