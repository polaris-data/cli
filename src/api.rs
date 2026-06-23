use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use reqwest::{Client, StatusCode, redirect};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Result, TickError};

#[derive(Debug, Clone)]
pub struct PolarisClient {
    base_url: String,
    api_key: Option<String>,
    api_client: Client,
    download_client: Client,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct CliAuthStartResponse {
    pub request_id: String,
    pub poll_token: String,
    pub user_code: String,
    pub login_url: String,
    pub expires_at: DateTime<Utc>,
    pub interval_ms: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CliAuthPollResponse {
    Pending {
        request_id: String,
        expires_at: DateTime<Utc>,
        interval_ms: u64,
    },
    Approved {
        request_id: String,
        user_id: String,
        display_name: Option<String>,
        email: Option<String>,
        wallet_address: Option<String>,
        avatar_url: Option<String>,
        api_key: String,
    },
    Consumed,
    Expired,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AccountResponse {
    pub user_id: String,
    pub auth: AccountAuth,
    pub identity: AccountIdentity,
    pub subscription: AccountSubscription,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct FeedbackResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AccountAuth {
    pub provider: String,
    pub key_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AccountIdentity {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub wallet_address: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AccountSubscription {
    pub tier: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogResponse {
    pub sources: Vec<CatalogSource>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogSource {
    pub id: String,
    pub markets: Vec<CatalogMarket>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CatalogMarket {
    pub id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    #[serde(rename = "source")]
    pub catalog_source: Option<String>,
    #[serde(
        default,
        alias = "category",
        deserialize_with = "deserialize_catalog_categories"
    )]
    pub categories: Vec<String>,
    #[serde(default)]
    pub access: Option<DatasetAccess>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogCategoriesWire {
    One(String),
    Many(Vec<String>),
}

fn deserialize_catalog_categories<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<CatalogCategoriesWire>::deserialize(deserializer)? {
            Some(CatalogCategoriesWire::One(category)) => vec![category],
            Some(CatalogCategoriesWire::Many(categories)) => categories,
            None => Vec::new(),
        },
    )
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct DatasetAccess {
    pub status: DatasetAccessStatus,
    #[serde(default)]
    pub public_cutoff_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DatasetAccessStatus {
    Open,
    Preview,
    Restricted,
}

impl DatasetAccess {
    pub fn search_text(&self) -> String {
        match self.status {
            DatasetAccessStatus::Open => "public".into(),
            DatasetAccessStatus::Restricted => "restricted".into(),
            DatasetAccessStatus::Preview => match self.public_cutoff_date {
                Some(date) => format!("preview {}", date),
                None => "preview".into(),
            },
        }
    }

    pub fn sort_order(&self) -> u8 {
        match self.status {
            DatasetAccessStatus::Open => 0,
            DatasetAccessStatus::Preview => 1,
            DatasetAccessStatus::Restricted => 2,
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            DatasetAccessStatus::Open => "public",
            DatasetAccessStatus::Preview => "preview",
            DatasetAccessStatus::Restricted => "restricted",
        }
    }

    pub fn requires_api_key_for_date(&self, selected_date: NaiveDate) -> bool {
        match self.status {
            DatasetAccessStatus::Open => false,
            DatasetAccessStatus::Restricted => true,
            DatasetAccessStatus::Preview => self
                .public_cutoff_date
                .is_none_or(|cutoff| selected_date < cutoff),
        }
    }

    pub fn summary_label(&self) -> String {
        match self.status {
            DatasetAccessStatus::Open => "public".into(),
            DatasetAccessStatus::Restricted => "restricted".into(),
            DatasetAccessStatus::Preview => match self.public_cutoff_date {
                Some(date) => format!("preview from {}", date),
                None => "preview".into(),
            },
        }
    }
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
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub key: String,
}

impl SnapshotEntryWire {
    fn into_snapshot(self) -> Result<SnapshotEntry> {
        Ok(SnapshotEntry {
            key: self.key,
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

    pub async fn start_cli_auth(&self) -> Result<CliAuthStartResponse> {
        let url = format!("{}/auth/cli/start", self.base_url);
        let response = self
            .api_client
            .post(url)
            .send()
            .await
            .context("CLI auth start request failed")
            .map_err(TickError::Other)?;
        decode_json_response(response, "CLI auth start request failed").await
    }

    pub async fn fetch_account(&self) -> Result<AccountResponse> {
        let url = format!("{}/account", self.base_url);
        let request = self.api_client.get(url);
        self.send_json(request, "account request failed").await
    }

    pub async fn submit_feedback(&self, message: &str) -> Result<FeedbackResponse> {
        let url = format!("{}/feedback", self.base_url);
        let request = self
            .api_client
            .post(url)
            .json(&serde_json::json!({ "message": message }));
        self.send_json(request, "feedback request failed").await
    }

    pub async fn poll_cli_auth(
        &self,
        request_id: &str,
        poll_token: &str,
    ) -> Result<CliAuthPollResponse> {
        let url = format!("{}/auth/cli/poll", self.base_url);
        let response = self
            .api_client
            .get(url)
            .query(&[("request_id", request_id), ("poll_token", poll_token)])
            .send()
            .await
            .context("CLI auth poll request failed")
            .map_err(TickError::Other)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("CLI auth poll request failed")
            .map_err(TickError::Other)?;

        if !(status.is_success() || status == StatusCode::GONE) {
            return Err(http_error(status, body, "CLI auth poll request failed"));
        }

        serde_json::from_str::<CliAuthPollResponse>(&body)
            .with_context(|| {
                format!(
                    "CLI auth poll request failed: failed to decode JSON response: {}",
                    body_snippet(&body)
                )
            })
            .map_err(TickError::Other)
    }

    pub async fn fetch_catalog(
        &self,
        source: Option<&str>,
        market: Option<&str>,
    ) -> Result<CatalogResponse> {
        if market.is_some() && source.is_none() {
            return Err(TickError::InvalidArgument(
                "--market on remote list requires --source".into(),
            ));
        }
        let url = format!("{}/catalog", self.base_url);
        let mut params: Vec<(&str, &str)> = Vec::new();
        if let Some(source) = source {
            params.push(("source", source));
        }
        if let Some(market) = market {
            params.push(("market", market));
        }
        let request = self.api_client.get(url).query(&params);
        self.send_json(request, "catalog request failed").await
    }

    pub async fn list_snapshots(
        &self,
        source: &str,
        market: &str,
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
                ("source".to_string(), source.to_string()),
                ("market".to_string(), market.to_string()),
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

    pub async fn download_snapshot(&self, key: &str) -> Result<reqwest::Response> {
        let url = format!("{}/snapshots/download", self.base_url);
        let response = self
            .authorized(
                self.download_client
                    .get(url)
                    .query(&[("key", key)]),
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
        decode_json_response(response, context).await
    }
}

async fn decode_json_response<T>(response: reqwest::Response, context: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
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
    use super::{
        CatalogResponse, DatasetAccessStatus, SnapshotEntryWire, StandardSnapshotsPageWire,
    };

    #[test]
    fn parses_current_snapshots_shape() {
        let page: StandardSnapshotsPageWire = serde_json::from_str(
            r#"{
                "source":"aster",
                "market":"ASTERUSDT",
                "total":1,
                "total_bytes":123,
                "limit":1000,
                "has_more":false,
                "next_cursor":null,
                "snapshots":[
                    {
                        "date":"2026-06-01",
                        "key":"snapshots/standard/aster/ASTERUSDT/2026-06-01.jsonl.zst"
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
    }

    #[test]
    fn parses_catalog_access_shape() {
        let catalog: CatalogResponse = serde_json::from_str(
            r#"{
                "sources":[
                    {
                        "id":"aster",
                        "markets":[
                            {
                                "id":"ASTERUSDT",
                                "start":"2026-05-18T14:16:33.886Z",
                                "end":"2026-06-03T13:10:01.561Z",
                                "source":"manifest",
                                "access":{
                                    "status":"preview",
                                    "public_cutoff_date":"2026-05-28"
                                }
                            }
                        ]
                    }
                ],
                "updatedAt":"2026-06-03T13:15:37.917Z"
            }"#,
        )
        .expect("catalog should parse");

        let access = catalog.sources[0].markets[0]
            .access
            .as_ref()
            .expect("access should parse");
        assert_eq!(access.status, DatasetAccessStatus::Preview);
        assert_eq!(
            access.public_cutoff_date.map(|value| value.to_string()),
            Some("2026-05-28".into())
        );
    }

    #[test]
    fn parses_catalog_categories_from_string_or_array() {
        let catalog: CatalogResponse = serde_json::from_str(
            r#"{
                "sources":[
                    {
                        "id":"aster",
                        "markets":[
                            {
                                "id":"ASTERUSDT",
                                "start":"2026-05-18T14:16:33.886Z",
                                "end":"2026-06-03T13:10:01.561Z",
                                "source":"manifest",
                                "category":"Bookmarks"
                            },
                            {
                                "id":"BTCUSDT",
                                "start":"2026-05-18T14:16:33.886Z",
                                "end":"2026-06-03T13:10:01.561Z",
                                "source":"manifest",
                                "categories":["Futures","Top Volume"]
                            }
                        ]
                    }
                ]
            }"#,
        )
        .expect("catalog should parse");

        assert_eq!(catalog.sources[0].markets[0].categories, vec!["Bookmarks"]);
        assert_eq!(
            catalog.sources[0].markets[1].categories,
            vec!["Futures", "Top Volume"]
        );
    }

    #[test]
    fn ignores_legacy_inline_download_urls() {
        let snapshot: SnapshotEntryWire = serde_json::from_str(
            r#"{
                "key":"bronze/aster/ASTERUSDT/2026-06-01/file.jsonl.zst",
                "downloadUrl":"https://example.test/file.jsonl.zst"
            }"#,
        )
        .expect("snapshot should parse");

        let snapshot = snapshot.into_snapshot().expect("snapshot should map");
        assert_eq!(
            snapshot.key,
            "bronze/aster/ASTERUSDT/2026-06-01/file.jsonl.zst"
        );
    }
}
