//! Wire schemas normalize legacy aliases before the runtime validates settings.
use super::*;
use serde::{Deserializer, de::DeserializeOwned};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy)]
struct Seconds(u64);
impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Number(u64),
            Text(String),
        }
        match Input::deserialize(deserializer)? {
            Input::Number(value) => Ok(Self(value)),
            Input::Text(text) => {
                let split = text
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(text.len());
                let (number, unit) = text.split_at(split);
                let multiplier = match unit {
                    "s" => 1,
                    "m" => 60,
                    "h" => 3600,
                    "d" => 86400,
                    "w" => 604800,
                    _ => {
                        return Err(serde::de::Error::custom(
                            "expected duration with s, m, h, d, or w suffix",
                        ));
                    }
                };
                number
                    .parse::<u64>()
                    .ok()
                    .and_then(|n| n.checked_mul(multiplier))
                    .map(Self)
                    .ok_or_else(|| serde::de::Error::custom("invalid duration"))
            }
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry {
    max_attempts: Option<u32>,
    base_delay: Option<Seconds>,
    max_delay: Option<Seconds>,
}
impl Retry {
    fn apply(self, policy: &mut DestinationPolicy) {
        if let Some(value) = self.max_attempts {
            policy.max_attempts = value;
        }
        if let Some(value) = self.base_delay {
            policy.retry_base_seconds = value.0;
        }
        if let Some(value) = self.max_delay {
            policy.retry_max_seconds = value.0;
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rate {
    attempts: u32,
    window: Seconds,
}
impl From<Rate> for RateLimit {
    fn from(value: Rate) -> Self {
        Self {
            attempts: value.attempts,
            window_seconds: value.window.0,
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    retry: Option<Retry>,
    rate_limit: Option<Rate>,
    group_window: Option<Seconds>,
    queue_limit: Option<u64>,
    max_attempts: Option<u32>,
    retry_base_seconds: Option<u64>,
    retry_max_seconds: Option<u64>,
    rate_limit_per_minute: Option<u32>,
    group_window_seconds: Option<u64>,
}
impl Delivery {
    fn normalize(self) -> Result<DeliveryConfig> {
        let mut d = DeliveryConfig::default();
        if self.retry.is_some()
            && (self.max_attempts.is_some()
                || self.retry_base_seconds.is_some()
                || self.retry_max_seconds.is_some())
        {
            bail!("delivery.retry: cannot be combined with legacy retry fields");
        }
        if self.rate_limit.is_some() && self.rate_limit_per_minute.is_some() {
            bail!("delivery.rate_limit: cannot be combined with rate_limit_per_minute");
        }
        if self.group_window.is_some() && self.group_window_seconds.is_some() {
            bail!("delivery.group_window: cannot be combined with group_window_seconds");
        }
        d.max_attempts = self.max_attempts.unwrap_or(d.max_attempts);
        d.retry_base_seconds = self.retry_base_seconds.unwrap_or(d.retry_base_seconds);
        d.retry_max_seconds = self.retry_max_seconds.unwrap_or(d.retry_max_seconds);
        if let Some(retry) = self.retry {
            let mut p = DestinationPolicy::from(&d);
            retry.apply(&mut p);
            d.max_attempts = p.max_attempts;
            d.retry_base_seconds = p.retry_base_seconds;
            d.retry_max_seconds = p.retry_max_seconds;
        }
        d.rate_limit_per_minute = self
            .rate_limit_per_minute
            .unwrap_or(d.rate_limit_per_minute);
        if let Some(rate) = self.rate_limit {
            d.rate_limit_per_minute = rate.attempts;
            d.rate_window_seconds = rate.window.0;
        }
        d.group_window_seconds = self
            .group_window
            .map(|s| s.0)
            .or(self.group_window_seconds)
            .unwrap_or(d.group_window_seconds);
        d.queue_limit = self.queue_limit.unwrap_or(d.queue_limit);
        Ok(d)
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    #[serde(default)]
    retry: Retry,
    rate_limit: Option<Rate>,
}
impl Overrides {
    fn normalize(self, global: &DeliveryConfig) -> DestinationPolicy {
        let mut policy = DestinationPolicy::from(global);
        self.retry.apply(&mut policy);
        policy.rate_limit = self.rate_limit.map(Into::into);
        policy
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Destination {
    Pushover {
        app_token: Option<Secret>,
        user_key: Option<Secret>,
        device: Option<String>,
        config: Option<PushoverConfig>,
        delivery: Option<Overrides>,
    },
    Webhook {
        url: Secret,
        bearer_token: Option<Secret>,
        delivery: Option<Overrides>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listener {
    #[serde(default = "listen")]
    listen: SocketAddr,
    api_token: Secret,
}
fn listen() -> SocketAddr {
    "127.0.0.1:8000".parse().unwrap()
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retention {
    deliveries: Option<Seconds>,
    idempotency_keys: Option<Seconds>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    #[serde(default = "database")]
    database: PathBuf,
    #[serde(default)]
    retention: Retention,
}
impl Default for Storage {
    fn default() -> Self {
        Self {
            database: database(),
            retention: Retention::default(),
        }
    }
}
fn database() -> PathBuf {
    "flare.sqlite3".into()
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Routing {
    default: Option<Vec<String>>,
    #[serde(default)]
    severity: BTreeMap<Severity, Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Server {
    server: Option<Listener>,
    storage: Option<Storage>,
    routing: Option<Routing>,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    destinations: BTreeMap<String, Destination>,
    host: Option<IpAddr>,
    port: Option<u16>,
    database: Option<PathBuf>,
    api_token: Option<Secret>,
    pushover: Option<PushoverConfig>,
    default_destinations: Option<Vec<String>>,
    routes: Option<BTreeMap<Severity, Vec<String>>>,
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)
        .map_err(|_| anyhow::anyhow!("configuration: cannot read UTF-8 YAML file"))?;
    serde_path_to_error::deserialize(serde_yaml_ng::Deserializer::from_str(&text)).map_err(|error| {
        // Never echo the parser's message: it may quote a credential or a full YAML value.
        let mut parts=Vec::new();
        for segment in error.path() {
            match segment {
                serde_path_to_error::Segment::Map {key}=> {
                    parts.push(if name_valid(key) {key.clone()} else {"<field>".into()});
                    if matches!(key.as_str(),"api_token"|"app_token"|"user_key"|"bearer_token"|"url") {break;}
                }
                serde_path_to_error::Segment::Seq {index}=>parts.push(format!("[{index}]")),
                _=>{}
            }
        }
        let field=if parts.is_empty() {"configuration".into()} else {parts.join(".")};
        let location=error.inner().location().map(|l|format!(" at line {}, column {}",l.line(),l.column())).unwrap_or_default();
        anyhow::anyhow!("{field}{location}: invalid configuration YAML; check field names, required fields, and value types")
    })
}
pub(super) fn server(path: &Path) -> Result<ServerConfig> {
    let raw: Server = read(path)?;
    let (host, port, api_token) = if let Some(server) = raw.server {
        if raw.host.is_some() || raw.port.is_some() || raw.api_token.is_some() {
            bail!("server: cannot be combined with legacy host, port, or api_token fields");
        }
        (server.listen.ip(), server.listen.port(), server.api_token)
    } else {
        (
            raw.host.unwrap_or_else(|| listen().ip()),
            raw.port.unwrap_or(8000),
            raw.api_token
                .ok_or_else(|| anyhow::anyhow!("server.api_token: required field is missing"))?,
        )
    };
    let storage = if let Some(storage) = raw.storage {
        if raw.database.is_some() {
            bail!("storage.database: cannot be combined with legacy database");
        }
        storage
    } else {
        Storage {
            database: raw.database.unwrap_or_else(database),
            ..Default::default()
        }
    };
    let routing = if let Some(routing) = raw.routing {
        if raw.default_destinations.is_some() || raw.routes.is_some() {
            bail!("routing: cannot be combined with legacy default_destinations or routes");
        }
        routing
    } else {
        Routing {
            default: raw.default_destinations,
            severity: raw.routes.unwrap_or_default(),
        }
    };
    let delivery = raw.delivery.normalize()?;
    let mut destinations = BTreeMap::new();
    let mut destination_policies = BTreeMap::new();
    for (name, destination) in raw.destinations {
        if !name_valid(&name) {
            bail!(
                "destinations: names must contain 1–64 ASCII letters, digits, underscores, or hyphens"
            );
        }
        let field = format!("destinations.{name}");
        let (destination, overrides) = match destination {
            Destination::Pushover {
                app_token,
                user_key,
                device,
                config,
                delivery,
            } => {
                let config = if let Some(config) = config {
                    if app_token.is_some() || user_key.is_some() || device.is_some() {
                        bail!("{field}: cannot combine config with flat Pushover fields");
                    }
                    config
                } else {
                    PushoverConfig {
                        app_token: app_token.ok_or_else(|| {
                            anyhow::anyhow!("{field}.app_token: required field is missing")
                        })?,
                        user_key: user_key.ok_or_else(|| {
                            anyhow::anyhow!("{field}.user_key: required field is missing")
                        })?,
                        device,
                    }
                };
                (DestinationConfig::Pushover { config }, delivery)
            }
            Destination::Webhook {
                url,
                bearer_token,
                delivery,
            } => (DestinationConfig::Webhook { url, bearer_token }, delivery),
        };
        destinations.insert(name.clone(), destination);
        if let Some(overrides) = overrides {
            destination_policies.insert(name, overrides.normalize(&delivery));
        }
    }
    let legacy_pushover = raw.pushover.is_some();
    if let Some(config) = raw.pushover {
        if destinations.contains_key("pushover") {
            bail!("destinations.pushover: conflicts with legacy pushover");
        }
        destinations.insert("pushover".into(), DestinationConfig::Pushover { config });
    }
    let defaults = routing.default.unwrap_or_else(|| {
        if legacy_pushover {
            vec!["pushover".into()]
        } else {
            vec![]
        }
    });
    Ok(ServerConfig {
        host,
        port,
        api_token,
        database: storage.database,
        delivery,
        destinations,
        default_destinations: defaults,
        routes: routing.severity,
        retention: RetentionConfig {
            deliveries: storage.retention.deliveries.map(|v| v.0),
            idempotency_keys: storage.retention.idempotency_keys.map(|v| v.0),
        },
        destination_policies,
    })
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Timeout {
    Number(f64),
    Text(Seconds),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    base_url: Option<String>,
    api_token: Secret,
    timeout: Option<Timeout>,
}
pub(super) fn client(path: &Path) -> Result<ClientConfig> {
    let raw: Client = read(path)?;
    Ok(ClientConfig {
        base_url: raw
            .base_url
            .unwrap_or_else(|| "http://127.0.0.1:8000".into()),
        api_token: raw.api_token,
        timeout: match raw.timeout {
            Some(Timeout::Number(value)) => value,
            Some(Timeout::Text(value)) => value.0 as f64,
            None => 15.0,
        },
    })
}
