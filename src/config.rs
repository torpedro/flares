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
            for (name, key) in [
                ("app_token", &pushover.app_token),
                ("user_key", &pushover.user_key),
            ] {
                if key.expose().len() != 30
                    || !key.expose().bytes().all(|b| b.is_ascii_alphanumeric())
                {
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
