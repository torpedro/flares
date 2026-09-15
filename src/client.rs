use std::time::Duration;

use reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;

use crate::{config::ClientConfig, models::*};

pub struct ApiClient {
    client: Client,
    config: ClientConfig,
}

impl ApiClient {
    pub fn new(config: ClientConfig) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self { client, config })
    }

    fn url(&self, path: &str) -> anyhow::Result<Url> {
        Url::parse(&format!(
            "{}{path}",
            self.config.base_url.trim_end_matches('/')
        ))
        .map_err(|_| anyhow::anyhow!("Invalid API base URL"))
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> anyhow::Result<T> {
        let response = request
            .bearer_auth(self.config.api_token.expose())
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "API request failed or timed out; delivery may have occurred. Reuse the alert idempotency key or check issue state before retrying"
                )
            })?;
        if !response.status().is_success() {
            // Do not echo untrusted response bodies, URLs, or credentials into terminal output.
            anyhow::bail!("API returned HTTP {}", response.status().as_u16());
        }
        response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("API returned an invalid response"))
    }

    pub async fn alert(&self, request: Alert, key: Option<String>) -> anyhow::Result<AlertResult> {
        request.validate().map_err(anyhow::Error::msg)?;
        let mut builder = self.client.post(self.url("/v1/alerts")?).json(&request);
        if let Some(key) = key {
            if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                anyhow::bail!("Invalid idempotency key");
            }
            builder = builder.header("Idempotency-Key", key);
        }
        self.send(builder).await
    }
    pub async fn delivery(&self, id: i64) -> anyhow::Result<Delivery> {
        self.send(self.client.get(self.url(&format!("/v1/deliveries/{id}"))?))
            .await
    }
    pub async fn register_heartbeat(&self, request: HeartbeatInput) -> anyhow::Result<Heartbeat> {
        request.validate().map_err(anyhow::Error::msg)?;
        self.send(self.client.post(self.url("/v1/heartbeats")?).json(&request))
            .await
    }
    pub async fn check_in(&self, id: String) -> anyhow::Result<Heartbeat> {
        validate_id(&id).map_err(anyhow::Error::msg)?;
        self.send(
            self.client
                .post(self.url("/v1/heartbeats/check-in")?)
                .json(&CloseIssue { id }),
        )
        .await
    }
    pub async fn heartbeats(&self) -> anyhow::Result<Vec<Heartbeat>> {
        self.send(self.client.get(self.url("/v1/heartbeats")?))
            .await
    }
    pub async fn delete_heartbeat(&self, id: String) -> anyhow::Result<()> {
        validate_id(&id).map_err(anyhow::Error::msg)?;
        let _: serde_json::Value = self
            .send(
                self.client
                    .delete(self.url("/v1/heartbeat")?)
                    .query(&[("id", id)]),
            )
            .await?;
        Ok(())
    }

    pub async fn open(&self, request: OpenIssue) -> anyhow::Result<MutationResult> {
        request.validate().map_err(anyhow::Error::msg)?;
        self.send(
            self.client
                .post(self.url("/v1/issues/open")?)
                .json(&request),
        )
        .await
    }

    pub async fn close(&self, id: String) -> anyhow::Result<MutationResult> {
        validate_id(&id).map_err(anyhow::Error::msg)?;
        self.send(
            self.client
                .post(self.url("/v1/issues/close")?)
                .json(&CloseIssue { id }),
        )
        .await
    }

    pub async fn get(&self, id: String) -> anyhow::Result<Issue> {
        validate_id(&id).map_err(anyhow::Error::msg)?;
        // Query encoding preserves even IDs such as ".", "..", or "a/../b" that URL
        // libraries and proxies would normalize when placed in the URL path.
        self.send(self.client.get(self.url("/v1/issue")?).query(&[("id", id)]))
            .await
    }

    pub async fn list(
        &self,
        status: Option<IssueStatus>,
        limit: u32,
        offset: u32,
    ) -> anyhow::Result<IssueList> {
        if !(1..=1000).contains(&limit) {
            anyhow::bail!("limit must be between 1 and 1000");
        }
        let mut params = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if let Some(status) = status {
            params.push(("status", status.as_str().into()));
        }
        self.send(self.client.get(self.url("/v1/issues")?).query(&params))
            .await
    }
}
