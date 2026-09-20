#![doc = include_str!("../README.md")]

use std::{path::Path, time::Duration};

pub mod config;

use reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;

pub use flares_types::*;

#[derive(Clone)]
pub struct ApiClient {
    client: Client,
    base_url: String,
    token: String,
}

impl ApiClient {
    /// Load one client YAML file. Relative secret paths use its canonical directory.
    pub fn from_config(path: impl AsRef<Path>) -> Result<Self, Error> {
        let config = config::ClientConfig::load(path.as_ref())
            .map_err(|error| Error::Configuration(error.to_string()))?;
        Self::with_timeout(
            config.base_url,
            config.api_token.expose(),
            Duration::from_secs_f64(config.timeout),
        )
    }

    /// Opt into XDG/HOME, then system client.yaml discovery.
    pub fn from_default_config() -> Result<Self, Error> {
        let path = config::default_path("client.yaml")
            .map_err(|error| Error::Configuration(error.to_string()))?;
        Self::from_config(path)
    }

    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self, Error> {
        Self::with_timeout(base_url, token, Duration::from_secs(15))
    }

    pub fn with_timeout(
        base_url: impl Into<String>,
        token: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let base_url = base_url.into();
        let token = token.into();
        let url = Url::parse(&base_url).map_err(|_| Error::Validation("Invalid API base URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Validation(
                "API base URL must be HTTP(S) without credentials, query, or fragment",
            ));
        }
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Validation(
                "API token must be nonempty printable ASCII without spaces",
            ));
        }
        if timeout.is_zero() || timeout > Duration::from_secs(86400) {
            return Err(Error::Validation(
                "Timeout must be greater than zero and at most 86400 seconds",
            ));
        }
        let client = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            client,
            base_url,
            token,
        })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        Url::parse(&format!("{}{path}", self.base_url.trim_end_matches('/')))
            .map_err(|_| Error::Validation("Invalid API base URL"))
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, Error> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            // Do not echo untrusted response bodies, URLs, or credentials into terminal output.
            return Err(Error::Http {
                status: response.status().as_u16(),
            });
        }
        let body = response.bytes().await.map_err(|_| Error::Transport)?;
        serde_json::from_slice(&body).map_err(|_| Error::Decode)
    }

    pub async fn alert(&self, request: Alert, key: Option<String>) -> Result<AlertResult, Error> {
        request.validate().map_err(Error::Validation)?;
        let mut builder = self.client.post(self.url("/v1/alerts")?).json(&request);
        if let Some(key) = key {
            if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::Validation("Invalid idempotency key"));
            }
            builder = builder.header("Idempotency-Key", key);
        }
        self.send(builder).await
    }
    pub async fn delivery(&self, id: i64) -> Result<Delivery, Error> {
        if id <= 0 {
            return Err(Error::Validation(
                "delivery id must be a positive 64-bit integer",
            ));
        }
        self.send(self.client.get(self.url(&format!("/v1/deliveries/{id}"))?))
            .await
    }
    pub async fn register_heartbeat(&self, request: HeartbeatInput) -> Result<Heartbeat, Error> {
        request.validate().map_err(Error::Validation)?;
        self.send(self.client.post(self.url("/v1/heartbeats")?).json(&request))
            .await
    }
    pub async fn check_in(&self, id: String) -> Result<Heartbeat, Error> {
        validate_id(&id).map_err(Error::Validation)?;
        self.send(
            self.client
                .post(self.url("/v1/heartbeats/check-in")?)
                .json(&CloseIssue { id }),
        )
        .await
    }
    pub async fn heartbeats(&self) -> Result<Vec<Heartbeat>, Error> {
        self.send(self.client.get(self.url("/v1/heartbeats")?))
            .await
    }
    pub async fn delete_heartbeat(&self, id: String) -> Result<(), Error> {
        validate_id(&id).map_err(Error::Validation)?;
        let response: serde_json::Value = self
            .send(
                self.client
                    .delete(self.url("/v1/heartbeat")?)
                    .query(&[("id", id)]),
            )
            .await?;
        if response.get("deleted") != Some(&serde_json::Value::Bool(true)) {
            return Err(Error::Decode);
        }
        Ok(())
    }

    pub async fn open(&self, request: OpenIssue) -> Result<MutationResult, Error> {
        request.validate().map_err(Error::Validation)?;
        self.send(
            self.client
                .post(self.url("/v1/issues/open")?)
                .json(&request),
        )
        .await
    }

    pub async fn close(&self, id: String) -> Result<MutationResult, Error> {
        validate_id(&id).map_err(Error::Validation)?;
        self.send(
            self.client
                .post(self.url("/v1/issues/close")?)
                .json(&CloseIssue { id }),
        )
        .await
    }

    pub async fn get(&self, id: String) -> Result<Issue, Error> {
        validate_id(&id).map_err(Error::Validation)?;
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
    ) -> Result<IssueList, Error> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Validation("limit must be between 1 and 1000"));
        }
        let mut params = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if let Some(status) = status {
            params.push(("status", status.as_str().into()));
        }
        self.send(self.client.get(self.url("/v1/issues")?).query(&params))
            .await
    }
}

/// Errors never include credentials, request URLs, or untrusted response bodies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Configuration(String),
    #[error("{0}")]
    Validation(&'static str),
    #[error(
        "API request failed or timed out; delivery may have occurred. Reuse the alert idempotency key or check issue state before retrying"
    )]
    Transport,
    #[error("API returned HTTP {status}")]
    Http { status: u16 },
    #[error("API returned an invalid response")]
    Decode,
}

impl ApiClient {
    pub async fn health(&self) -> Result<Health, Error> {
        self.send(self.client.get(self.url("/healthz")?)).await
    }
    pub async fn readiness(&self) -> Result<Health, Error> {
        self.send(self.client.get(self.url("/readyz")?)).await
    }
    pub async fn metrics(&self) -> Result<String, Error> {
        let response = self
            .client
            .get(self.url("/metrics")?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Http {
                status: response.status().as_u16(),
            });
        }
        response.text().await.map_err(|_| Error::Transport)
    }
}
