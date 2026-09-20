//! Configuration loading has no database or network side effects.
use crate::models::Severity;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    net::IpAddr,
    path::{Path, PathBuf},
};
mod file;

/// Search the user's configuration directory, then the system directory.
pub fn default_path(filename: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    find_config(filename, home.as_deref(), Path::new("/etc/flares"))
}

fn find_config(filename: &str, home: Option<&Path>, system: &Path) -> Result<PathBuf> {
    let user = home.map(|home| home.join(".config/flares").join(filename));
    for path in user
        .into_iter()
        .chain(std::iter::once(system.join(filename)))
    {
        if path.try_exists().map_err(|_| {
            anyhow::anyhow!(
                "Cannot access configuration {}; use --config to specify a file",
                path.display()
            )
        })? {
            return Ok(path);
        }
    }
    bail!("No {filename} found in ~/.config/flares or /etc/flares; use --config to specify a file")
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn user_config_takes_precedence_and_missing_files_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let user = home.join(".config/flares");
        let system = root.path().join("etc/flares");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&system).unwrap();
        for name in ["server.yaml", "client.yaml"] {
            let path = system.join(name);
            fs::write(&path, "system").unwrap();
            assert_eq!(find_config(name, Some(&home), &system).unwrap(), path);
            assert_eq!(find_config(name, None, &system).unwrap(), path);
            let path = user.join(name);
            // An invalid user file must be selected, not silently bypassed.
            fs::write(&path, "invalid YAML [").unwrap();
            assert_eq!(find_config(name, Some(&home), &system).unwrap(), path);
            assert!(ServerConfig::load(&path).is_err());
        }
    }

    #[test]
    fn missing_config_reports_search_locations_and_does_not_create_directories() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("etc/flares");
        let error = find_config("server.yaml", Some(&home), &system)
            .unwrap_err()
            .to_string();
        for expected in ["server.yaml", "~/.config/flares", "/etc/flares", "--config"] {
            assert!(error.contains(expected));
        }
        assert!(!home.exists());
        assert!(!system.exists());
    }
}

#[derive(Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Secret {
    Literal(String),
    Environment { env: String },
    File { file: PathBuf },
}
impl Secret {
    pub fn expose(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            _ => panic!("Secret reference must be resolved by configuration loading"),
        }
    }
    fn resolve(&mut self, directory: &Path, field: &str) -> Result<()> {
        let value = match self {
            Self::Literal(_) => return Ok(()),
            Self::Environment { env } => std::env::var(env).map_err(|_| {
                anyhow::anyhow!("{field}: environment variable is missing or not UTF-8")
            })?,
            Self::File { file } => fs::read_to_string(directory.join(file))
                .map_err(|_| anyhow::anyhow!("{field}: cannot read secret file as UTF-8"))?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        };
        *self = Self::Literal(value);
        Ok(())
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PushoverConfig {
    pub app_token: Secret,
    pub user_key: Secret,
    pub device: Option<String>,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub database: PathBuf,
    pub api_token: Secret,
    pub delivery: DeliveryConfig,
    pub destinations: BTreeMap<String, DestinationConfig>,
    pub default_destinations: Vec<String>,
    pub routes: BTreeMap<Severity, Vec<String>>,
    pub retention: RetentionConfig,
    pub destination_policies: BTreeMap<String, DestinationPolicy>,
}
#[derive(Debug)]
pub struct ClientConfig {
    pub base_url: String,
    pub api_token: Secret,
    pub timeout: f64,
}

#[derive(Debug, Clone)]
pub enum DestinationConfig {
    Pushover {
        config: PushoverConfig,
    },
    Webhook {
        url: Secret,
        bearer_token: Option<Secret>,
    },
}

#[derive(Debug, Clone)]
pub struct DeliveryConfig {
    pub max_attempts: u32,
    pub retry_base_seconds: u64,
    pub retry_max_seconds: u64,
    // Retained internally for existing consumers; the rate window is configurable.
    pub rate_limit_per_minute: u32,
    pub rate_window_seconds: u64,
    pub group_window_seconds: u64,
    pub queue_limit: u64,
}
impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            retry_base_seconds: 10,
            retry_max_seconds: 3600,
            rate_limit_per_minute: 120,
            rate_window_seconds: 60,
            group_window_seconds: 30,
            queue_limit: 10_000,
        }
    }
}
#[derive(Debug, Default, Clone)]
pub struct RetentionConfig {
    pub deliveries: Option<u64>,
    pub idempotency_keys: Option<u64>,
}
#[derive(Debug, Clone)]
pub struct RateLimit {
    pub attempts: u32,
    pub window_seconds: u64,
}
#[derive(Debug, Clone)]
pub struct DestinationPolicy {
    pub max_attempts: u32,
    pub retry_base_seconds: u64,
    pub retry_max_seconds: u64,
    pub rate_limit: Option<RateLimit>,
}
impl From<&DeliveryConfig> for DestinationPolicy {
    fn from(value: &DeliveryConfig) -> Self {
        Self {
            max_attempts: value.max_attempts,
            retry_base_seconds: value.retry_base_seconds,
            retry_max_seconds: value.retry_max_seconds,
            rate_limit: None,
        }
    }
}
fn validate_retry(attempts: u32, base: u64, max: u64, field: &str) -> Result<()> {
    if !(1..=20).contains(&attempts) {
        bail!("{field}.max_attempts: must be between 1 and 20");
    }
    if !(1..=86400).contains(&base) {
        bail!("{field}.base_delay: must be between 1s and 1d");
    }
    if max < base {
        bail!("{field}.max_delay: must be at least base_delay");
    }
    if max > 86400 {
        bail!("{field}.max_delay: must be at most 1d");
    }
    Ok(())
}
fn validate_rate(window: u64, field: &str) -> Result<()> {
    if !(1..=86400).contains(&window) {
        bail!("{field}.window: must be between 1s and 1d");
    }
    Ok(())
}
fn token(secret: &Secret, field: &str) -> Result<()> {
    if secret.expose().is_empty() || !secret.expose().bytes().all(|b| b.is_ascii_graphic()) {
        bail!("{field}: must be nonempty printable ASCII without whitespace");
    }
    Ok(())
}
fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn validate_pushover(config: &mut PushoverConfig, directory: &Path, field: &str) -> Result<()> {
    for (name, secret) in [
        ("app_token", &mut config.app_token),
        ("user_key", &mut config.user_key),
    ] {
        let field = format!("{field}.{name}");
        secret.resolve(directory, &field)?;
        if secret.expose().len() != 30
            || !secret.expose().bytes().all(|b| b.is_ascii_alphanumeric())
        {
            bail!("{field}: must contain 30 ASCII letters or digits");
        }
    }
    if config.device.as_ref().is_some_and(|s| {
        s.is_empty()
            || s.len() > 25
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        bail!("{field}.device: must contain 1–25 letters, digits, underscores, or hyphens");
    }
    Ok(())
}
fn directory(path: &Path) -> Result<PathBuf> {
    Ok(path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("configuration: cannot resolve file directory"))?
        .parent()
        .expect("file has parent")
        .to_owned())
}
impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = file::server(path)?;
        let directory = directory(path)?;
        if config.port == 0 {
            bail!("server.listen: port must be between 1 and 65535");
        }
        if config.database.as_os_str().is_empty() || config.database == Path::new(":memory:") {
            bail!("storage.database: must name a persistent SQLite file");
        }
        if config.database.is_relative() {
            config.database = directory.join(&config.database);
        }
        config.api_token.resolve(&directory, "server.api_token")?;
        token(&config.api_token, "server.api_token")?;
        let d = &config.delivery;
        validate_retry(
            d.max_attempts,
            d.retry_base_seconds,
            d.retry_max_seconds,
            "delivery.retry",
        )?;
        validate_rate(d.rate_window_seconds, "delivery.rate_limit")?;
        if d.group_window_seconds > 86400 {
            bail!("delivery.group_window: must be between 0s and 1d");
        }
        if d.queue_limit > i64::MAX as u64 {
            bail!("delivery.queue_limit: exceeds the supported maximum");
        }
        for (field, value) in [
            ("deliveries", config.retention.deliveries),
            ("idempotency_keys", config.retention.idempotency_keys),
        ] {
            if value.is_some_and(|v| v == 0 || v > 315_360_000) {
                bail!("storage.retention.{field}: must be null or between 1s and 3650d");
            }
        }
        for (name, destination) in &mut config.destinations {
            if !name_valid(name) {
                bail!(
                    "destinations: names must contain 1–64 ASCII letters, digits, underscores, or hyphens"
                );
            }
            let field = format!("destinations.{name}");
            match destination {
                DestinationConfig::Pushover { config } => {
                    validate_pushover(config, &directory, &field)?
                }
                DestinationConfig::Webhook { url, bearer_token } => {
                    url.resolve(&directory, &format!("{field}.url"))?;
                    let parsed = reqwest::Url::parse(url.expose())
                        .map_err(|_| anyhow::anyhow!("{field}.url: invalid webhook URL"))?;
                    if !matches!(parsed.scheme(), "http" | "https")
                        || parsed.host_str().is_none()
                        || !parsed.username().is_empty()
                        || parsed.password().is_some()
                        || parsed.fragment().is_some()
                    {
                        bail!("{field}.url: must be HTTP(S) without credentials or a fragment");
                    }
                    if let Some(secret) = bearer_token {
                        secret.resolve(&directory, &format!("{field}.bearer_token"))?;
                        token(secret, &format!("{field}.bearer_token"))?;
                    }
                }
            }
            if let Some(policy) = config.destination_policies.get(name) {
                validate_retry(
                    policy.max_attempts,
                    policy.retry_base_seconds,
                    policy.retry_max_seconds,
                    &format!("{field}.delivery.retry"),
                )?;
                if let Some(rate) = &policy.rate_limit {
                    validate_rate(rate.window_seconds, &format!("{field}.delivery.rate_limit"))?;
                }
            }
        }
        for (field, names) in
            std::iter::once(("routing.default".to_owned(), &config.default_destinations)).chain(
                config.routes.iter().map(|(severity, names)| {
                    (
                        format!(
                            "routing.severity.{}",
                            serde_json::to_value(severity).unwrap().as_str().unwrap()
                        ),
                        names,
                    )
                }),
            )
        {
            let mut seen = BTreeSet::new();
            for name in names {
                if !config.destinations.contains_key(name) {
                    bail!("{field}: references an unknown destination");
                }
                if !seen.insert(name) {
                    bail!("{field}: contains a duplicate destination");
                }
            }
        }
        Ok(config)
    }
    /// Canonical effective configuration. All secrets, including webhook URLs, are redacted.
    pub fn effective(&self) -> serde_json::Value {
        use serde_json::json;
        let d = &self.delivery;
        let mut destinations = serde_json::Map::new();
        for (name, destination) in &self.destinations {
            let mut value = match destination {
                DestinationConfig::Pushover { config } => {
                    json!({"type":"pushover","app_token":config.app_token,"user_key":config.user_key,"device":config.device})
                }
                DestinationConfig::Webhook { url, bearer_token } => {
                    json!({"type":"webhook","url":url,"bearer_token":bearer_token})
                }
            };
            let policy = self
                .destination_policies
                .get(name)
                .cloned()
                .unwrap_or_else(|| DestinationPolicy::from(d));
            value["delivery"] = json!({"retry":retry_value(policy.max_attempts,policy.retry_base_seconds,policy.retry_max_seconds),"rate_limit":policy.rate_limit.map(|r|json!({"attempts":r.attempts,"window":format!("{}s",r.window_seconds)}))});
            destinations.insert(name.clone(), value);
        }
        json!({"server":{"listen":std::net::SocketAddr::new(self.host,self.port).to_string(),"api_token":self.api_token},
            "storage":{"database":self.database,"retention":{"deliveries":self.retention.deliveries.map(|v|format!("{v}s")),"idempotency_keys":self.retention.idempotency_keys.map(|v|format!("{v}s"))}},
            "delivery":{"retry":retry_value(d.max_attempts,d.retry_base_seconds,d.retry_max_seconds),"rate_limit":{"attempts":d.rate_limit_per_minute,"window":format!("{}s",d.rate_window_seconds)},"group_window":format!("{}s",d.group_window_seconds),"queue_limit":d.queue_limit},
            "destinations":destinations,"routing":{"default":self.default_destinations,"severity":self.routes}})
    }
}
fn retry_value(attempts: u32, base: u64, max: u64) -> serde_json::Value {
    serde_json::json!({"max_attempts":attempts,"base_delay":format!("{base}s"),"max_delay":format!("{max}s")})
}
impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = file::client(path)?;
        config.api_token.resolve(&directory(path)?, "api_token")?;
        token(&config.api_token, "api_token")?;
        let url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| anyhow::anyhow!("base_url: invalid HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("base_url: must be HTTP(S) without credentials, query, or fragment");
        }
        if !config.timeout.is_finite() || config.timeout <= 0.0 || config.timeout > 86400.0 {
            bail!("timeout: must be greater than 0s and at most 1d");
        }
        Ok(config)
    }
    pub fn effective(&self) -> serde_json::Value {
        serde_json::json!({"base_url":self.base_url,"api_token":self.api_token,"timeout":self.timeout})
    }
}
