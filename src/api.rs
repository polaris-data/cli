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

#[derive(Debug, Deserialize, Clone)]
struct SnapshotsPage {
    pub total: usize,
    pub total_bytes: u64,
    pub next_cursor: Option<String>,
    pub snapshots: Vec<SnapshotEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub key: String,
    pub filename: String,
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
            let page: SnapshotsPage = self.send_json(request, "snapshot listing failed").await?;
            if all.is_empty() {
                total_bytes = page.total_bytes;
            }
            all.extend(page.snapshots);

            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                if page.total != all.len() && page.total != 0 {
                    return Err(TickError::Other(anyhow!(
                        "snapshot pagination returned {} entries but advertised {}",
                        all.len(),
                        page.total
                    )));
                }
                break;
            }
        }

        Ok((all, total_bytes))
    }

    pub async fn fetch_download_url(&self, key: &str) -> Result<String> {
        let url = format!("{}/snapshots/download", self.base_url);
        let response = self
            .authorized(self.api_client.get(url))
            .query(&[("key", key)])
            .send()
            .await
            .context("snapshot download URL request failed")
            .map_err(TickError::Other)?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| {
                    TickError::Other(anyhow!("snapshot redirect missing location header"))
                })?;
            let value = location
                .to_str()
                .context("snapshot redirect location was not valid UTF-8")
                .map_err(TickError::Other)?;
            return Ok(value.to_string());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(http_error(
            status,
            body,
            "snapshot download URL request failed",
        ))
    }

    pub async fn download(&self, url: &str) -> Result<reqwest::Response> {
        let response = self
            .download_client
            .get(url)
            .send()
            .await
            .with_context(|| format!("download request failed for {url}"))
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
        response
            .json::<T>()
            .await
            .with_context(|| context.to_string())
            .map_err(TickError::Other)
    }
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
