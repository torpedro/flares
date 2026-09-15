use std::{
    fmt, fs,
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, de::DeserializeOwned};

#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushoverConfig {
    pub app_token: Secret,
    pub user_key: Secret,
    pub device: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_database")]
    pub database: PathBuf,
    pub api_token: Secret,
    pub pushover: Option<PushoverConfig>,
    #[serde(default)]
    pub delivery: DeliveryConfig,
    #[serde(default)]
    pub destinations: std::collections::BTreeMap<String, DestinationConfig>,
    #[serde(default)]
    pub default_destinations: Vec<String>,
    #[serde(default)]
    pub routes: std::collections::BTreeMap<crate::models::Severity, Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    #[serde(default = "default_url")]
    pub base_url: String,
    pub api_token: Secret,
    #[serde(default = "default_timeout")]
    pub timeout: f64,
}

fn default_host() -> IpAddr {
    IpAddr::from([127, 0, 0, 1])
}
fn default_port() -> u16 {
    8000
}
fn default_database() -> PathBuf {
    "flare.sqlite3".into()
}
fn default_url() -> String {
    "http://127.0.0.1:8000".into()
}
fn default_timeout() -> f64 {
    15.0
}

fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).map_err(|_| {
        anyhow::anyhow!("Cannot read configuration: use a readable UTF-8 YAML file")
    })?;
    // Parser errors can contain input values, including secrets. Never display them.
    serde_yaml_ng::from_str(&text).map_err(|_| {
        anyhow::anyhow!("Invalid configuration YAML: check required fields, names, and types")
    })
}

fn validate_token(token: &Secret) -> Result<()> {
    if token.expose().is_empty() || !token.expose().bytes().all(|b| b.is_ascii_graphic()) {
        bail!("api_token must be nonempty printable ASCII without whitespace");
    }
    Ok(())
}

impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self = load(path)?;
        validate_token(&config.api_token)?;
        if config.port == 0 {
            bail!("port must be between 1 and 65535");
        }
        if config.database.as_os_str().is_empty() || config.database == Path::new(":memory:") {
            bail!("database must name a persistent SQLite file");
        }
        if let Some(pushover) = &config.pushover {
            validate_pushover(pushover)?;
        }
        let delivery = &config.delivery;
        if !(1..=20).contains(&delivery.max_attempts)
            || !(1..=86400).contains(&delivery.retry_base_seconds)
            || !(delivery.retry_base_seconds..=86400).contains(&delivery.retry_max_seconds)
            || delivery.group_window_seconds > 86400
        {
            bail!(
                "Invalid delivery limits: attempts 1–20, retry delays 1–86400, grouping 0–86400 seconds"
            );
        }
        for (name, destination) in &config.destinations {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                bail!(
                    "Destination names must contain 1–64 letters, digits, underscores, or hyphens"
                );
            }
            if name == "pushover" && config.pushover.is_some() {
                bail!("Destination name pushover is already in use");
            }
            match destination {
                DestinationConfig::Pushover { config } => validate_pushover(config)?,
                DestinationConfig::Webhook { url, bearer_token } => {
                    let url = reqwest::Url::parse(url.expose())
                        .map_err(|_| anyhow::anyhow!("Invalid webhook URL"))?;
                    if !matches!(url.scheme(), "http" | "https")
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                    {
                        bail!(
                            "Webhook URL must be HTTP or HTTPS without credentials or a fragment"
                        );
                    }
                    if let Some(token) = bearer_token {
                        validate_token(token)?;
                    }
                }
            }
        }
        if config.default_destinations.is_empty() && config.pushover.is_some() {
            config.default_destinations.push("pushover".into());
        }
        for names in std::iter::once(&config.default_destinations).chain(config.routes.values()) {
            let mut seen = std::collections::BTreeSet::new();
            for name in names {
                if !(config.destinations.contains_key(name)
                    || (name == "pushover" && config.pushover.is_some()))
                {
                    bail!("Routing references an unknown destination");
                }
                if !seen.insert(name) {
                    bail!("Routing contains a duplicate destination");
                }
            }
        }
        if config.database.is_relative() {
            let directory = path
                .canonicalize()
                .map_err(|_| anyhow::anyhow!("Cannot resolve configuration directory"))?;
            config.database = directory
                .parent()
                .expect("file has parent")
                .join(&config.database);
        }
        Ok(config)
    }
}

impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = load(path)?;
        validate_token(&config.api_token)?;
        let url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| anyhow::anyhow!("base_url must be an HTTP or HTTPS URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("base_url must be HTTP or HTTPS without credentials, a query, or a fragment");
        }
        if !config.timeout.is_finite() || config.timeout <= 0.0 || config.timeout > 86400.0 {
            bail!("timeout must be greater than zero and at most 86400 seconds");
        }
        Ok(config)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryConfig {
    pub max_attempts: u32,
    pub retry_base_seconds: u64,
    pub retry_max_seconds: u64,
    pub rate_limit_per_minute: u32,
    pub group_window_seconds: u64,
}
impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            retry_base_seconds: 10,
            retry_max_seconds: 3600,
            rate_limit_per_minute: 120,
            group_window_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DestinationConfig {
    Pushover {
        config: PushoverConfig,
    },
    Webhook {
        url: Secret,
        bearer_token: Option<Secret>,
    },
}

fn validate_pushover(pushover: &PushoverConfig) -> Result<()> {
    for (name, key) in [
        ("app_token", &pushover.app_token),
        ("user_key", &pushover.user_key),
    ] {
        if key.expose().len() != 30 || !key.expose().bytes().all(|b| b.is_ascii_alphanumeric()) {
            bail!("pushover.{name} must be a 30-character alphanumeric key");
        }
    }
    if pushover.device.as_ref().is_some_and(|device| {
        device.is_empty()
            || device.len() > 25
            || !device
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        bail!("pushover.device must contain 1–25 letters, digits, underscores, or hyphens");
    }
    Ok(())
}
