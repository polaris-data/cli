use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Client, StatusCode, redirect};
use serde::{Deserialize, Serialize};

use crate::error::{Result, TickError};

#[derive(Debug, Clone)]
pub struct PolarisClient {
    base_url: String,
    api_key: Option<String>,
    api_client: Client,
    download_client: Client,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogResponse {
    pub exchanges: Vec<CatalogExchange>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogExchange {
    pub id: String,
    pub assets: Vec<CatalogAsset>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogAsset {
    pub id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
struct SnapshotsPage {
    pub total: Option<usize>,
    pub total_bytes: u64,
    pub next_cursor: Option<String>,
    pub snapshots: Vec<SnapshotEntry>,
}

#[derive(Debug, Deserialize)]
struct StandardSnapshotsPageWire {
    total: Option<usize>,
    #[serde(default)]
    total_bytes: Option<u64>,
    next_cursor: Option<String>,
    #[serde(default)]
    data: Vec<SnapshotEntryWire>,
    #[serde(default)]
    snapshots: Vec<SnapshotEntryWire>,
}

#[derive(Debug, Deserialize)]
struct SnapshotEntryWire {
    #[serde(alias = "path", alias = "name")]
    key: String,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub key: String,
    pub filename: String,
}

impl SnapshotEntryWire {
    fn into_snapshot(self) -> Result<SnapshotEntry> {
        let filename = self
            .filename
            .or_else(|| self.key.rsplit('/').next().map(str::to_string))
            .ok_or_else(|| {
                TickError::Other(anyhow!("snapshot entry did not include a filename"))
            })?;

        Ok(SnapshotEntry {
            key: self.key,
            filename,
        })
    }
}

impl PolarisClient {
    pub fn new(base_url: String, api_key: Option<String>, timeout: Duration) -> Result<Self> {
        let api_client = Client::builder()
            .timeout(timeout)
            .redirect(redirect::Policy::none())
            .build()
            .context("failed to build API client")
            .map_err(TickError::Other)?;
        let download_client = Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build download client")
            .map_err(TickError::Other)?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            api_client,
            download_client,
        })
    }

    pub fn download_client(&self) -> &Client {
        &self.download_client
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub async fn fetch_catalog(
        &self,
        exchange: Option<&str>,
        asset: Option<&str>,
    ) -> Result<CatalogResponse> {
        if asset.is_some() && exchange.is_none() {
            return Err(TickError::InvalidArgument(
                "--asset on remote list requires --exchange".into(),
            ));
        }
        let url = format!("{}/catalog", self.base_url);
        let mut params: Vec<(&str, &str)> = Vec::new();
        if let Some(exchange) = exchange {
            params.push(("exchange", exchange));
        }
        if let Some(asset) = asset {
            params.push(("asset", asset));
        }
        let request = self.api_client.get(url).query(&params);
        self.send_json(request, "catalog request failed").await
    }

    pub async fn list_snapshots(
        &self,
        exchange: &str,
        asset: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<(Vec<SnapshotEntry>, u64)> {
        let mut cursor: Option<String> = None;
        let mut all = Vec::new();
        let mut total_bytes = 0u64;

        loop {
            let url = format!("{}/snapshots", self.base_url);
            let from_text = to_rfc3339(from);
            let to_text = to_rfc3339(to);
            let mut params = vec![
                ("exchange".to_string(), exchange.to_string()),
                ("asset".to_string(), asset.to_string()),
                ("from".to_string(), from_text),
                ("to".to_string(), to_text),
                ("limit".to_string(), "1000".to_string()),
            ];
            if let Some(value) = cursor.clone() {
                params.push(("cursor".to_string(), value));
            }

            let request = self.api_client.get(url).query(&params);
            let page = self
                .send_json::<StandardSnapshotsPageWire>(request, "snapshot listing failed")
                .await?;
            let mut snapshot_entries = page.snapshots;
            if !page.data.is_empty() {
                snapshot_entries.extend(page.data);
            }
            let page = SnapshotsPage {
                total: page.total,
                total_bytes: page.total_bytes.unwrap_or(0),
                next_cursor: page.next_cursor,
                snapshots: snapshot_entries
                    .into_iter()
                    .map(SnapshotEntryWire::into_snapshot)
                    .collect::<Result<Vec<_>>>()?,
            };
            if all.is_empty() {
                total_bytes = page.total_bytes;
            }
            all.extend(page.snapshots);

            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                if let Some(total) = page.total
                    && total != all.len()
                    && total != 0
                {
                    return Err(TickError::Other(anyhow!(
                        "snapshot pagination returned {} entries but advertised {}",
                        all.len(),
                        total
                    )));
                }
                break;
            }
        }

        Ok((all, total_bytes))
    }

    pub async fn download_snapshot(&self, key: &str, filename: &str) -> Result<reqwest::Response> {
        let url = format!("{}/snapshots/download", self.base_url);
        let response = self
            .authorized(
                self.download_client
                    .get(url)
                    .query(&[("key", key), ("filename", filename)]),
            )
            .send()
            .await
            .with_context(|| format!("snapshot download request failed for {key}"))
            .map_err(TickError::Other)?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body = response.text().await.unwrap_or_default();
        Err(http_error(status, body, "snapshot download failed"))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
    }

    async fn send_json<T>(&self, request: reqwest::RequestBuilder, context: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .authorized(request)
            .send()
            .await
            .with_context(|| context.to_string())
            .map_err(TickError::Other)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, body, context));
        }
        let body = response
            .text()
            .await
            .with_context(|| context.to_string())
            .map_err(TickError::Other)?;
        serde_json::from_str::<T>(&body)
            .with_context(|| {
                format!(
                    "{context}: failed to decode JSON response: {}",
                    body_snippet(&body)
                )
            })
            .map_err(TickError::Other)
    }
}

fn body_snippet(body: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut snippet = body.trim().chars().take(MAX_CHARS + 1).collect::<String>();
    if snippet.is_empty() {
        return "<empty body>".into();
    }
    if snippet.chars().count() > MAX_CHARS {
        snippet = snippet.chars().take(MAX_CHARS).collect::<String>();
        snippet.push_str("...");
    }
    snippet
}

fn http_error(status: StatusCode, body: String, context: &str) -> TickError {
    let message = if body.trim().is_empty() {
        format!("{context} ({status})")
    } else {
        format!("{context} ({status}): {}", body.trim())
    };
    TickError::Request {
        status: Some(status),
        retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
        message,
    }
}

fn to_rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::{SnapshotEntryWire, StandardSnapshotsPageWire};

    #[test]
    fn parses_current_snapshots_shape() {
        let page: StandardSnapshotsPageWire = serde_json::from_str(
            r#"{
                "exchange":"aster",
                "asset":"ASTERUSDT",
                "total":1,
                "total_bytes":123,
                "limit":1000,
                "has_more":false,
                "next_cursor":null,
                "snapshots":[
                    {
                        "date":"2026-06-01",
                        "key":"snapshots/standard/aster/ASTERUSDT/2026-06-01.jsonl.zst",
                        "filename":"aster_ASTERUSDT_2026-06-01_standard.jsonl.zst"
                    }
                ]
            }"#,
        )
        .expect("page should parse");

        assert_eq!(page.snapshots.len(), 1);
        let snapshot = page
            .snapshots
            .into_iter()
            .next()
            .expect("snapshot entry")
            .into_snapshot()
            .expect("snapshot should map");
        assert_eq!(
            snapshot.key,
            "snapshots/standard/aster/ASTERUSDT/2026-06-01.jsonl.zst"
        );
        assert_eq!(
            snapshot.filename,
            "aster_ASTERUSDT_2026-06-01_standard.jsonl.zst"
        );
    }

    #[test]
    fn ignores_legacy_inline_download_urls() {
        let snapshot: SnapshotEntryWire = serde_json::from_str(
            r#"{
                "key":"bronze/aster/ASTERUSDT/2026-06-01/file.jsonl.zst",
                "filename":"file.jsonl.zst",
                "downloadUrl":"https://example.test/file.jsonl.zst"
            }"#,
        )
        .expect("snapshot should parse");

        let snapshot = snapshot.into_snapshot().expect("snapshot should map");
        assert_eq!(
            snapshot.key,
            "bronze/aster/ASTERUSDT/2026-06-01/file.jsonl.zst"
        );
        assert_eq!(snapshot.filename, "file.jsonl.zst");
    }
}
