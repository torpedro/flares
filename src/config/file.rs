//! Strict YAML schemas for the structured configuration format.
use super::*;
use serde::{Deserializer, de::DeserializeOwned};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy)]
struct Seconds(u64);
impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
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
}
impl Delivery {
    fn resolve(self) -> DeliveryConfig {
        let mut d = DeliveryConfig::default();
        if let Some(retry) = self.retry {
            let mut p = DestinationPolicy::from(&d);
            retry.apply(&mut p);
            d.max_attempts = p.max_attempts;
            d.retry_base_seconds = p.retry_base_seconds;
            d.retry_max_seconds = p.retry_max_seconds;
        }
        if let Some(rate) = self.rate_limit {
            d.rate_limit_per_minute = rate.attempts;
            d.rate_window_seconds = rate.window.0;
        }
        d.group_window_seconds = self
            .group_window
            .map(|s| s.0)
            .unwrap_or(d.group_window_seconds);
        d.queue_limit = self.queue_limit.unwrap_or(d.queue_limit);
        d
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
        app_token: Secret,
        user_key: Secret,
        device: Option<String>,
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
    "flares.sqlite3".into()
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Routing {
    #[serde(default)]
    default: Vec<String>,
    #[serde(default)]
    severity: BTreeMap<Severity, Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Server {
    server: Listener,
    #[serde(default)]
    storage: Storage,
    #[serde(default)]
    routing: Routing,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    destinations: BTreeMap<String, Destination>,
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
    let Listener { listen, api_token } = raw.server;
    let storage = raw.storage;
    let routing = raw.routing;
    let delivery = raw.delivery.resolve();
    let mut destinations = BTreeMap::new();
    let mut destination_policies = BTreeMap::new();
    for (name, destination) in raw.destinations {
        if !name_valid(&name) {
            bail!(
                "destinations: names must contain 1–64 ASCII letters, digits, underscores, or hyphens"
            );
        }
        let (destination, overrides) = match destination {
            Destination::Pushover {
                app_token,
                user_key,
                device,
                delivery,
            } => (
                DestinationConfig::Pushover {
                    config: PushoverConfig {
                        app_token,
                        user_key,
                        device,
                    },
                },
                delivery,
            ),
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
    Ok(ServerConfig {
        host: listen.ip(),
        port: listen.port(),
        api_token,
        database: storage.database,
        delivery,
        destinations,
        default_destinations: routing.default,
        routes: routing.severity,
        retention: RetentionConfig {
            deliveries: storage.retention.deliveries.map(|v| v.0),
            idempotency_keys: storage.retention.idempotency_keys.map(|v| v.0),
        },
        destination_policies,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    base_url: Option<String>,
    api_token: Secret,
    timeout: Option<Seconds>,
}
pub(super) fn client(path: &Path) -> Result<ClientConfig> {
    let raw: Client = read(path)?;
    Ok(ClientConfig {
        base_url: raw
            .base_url
            .unwrap_or_else(|| "http://127.0.0.1:8000".into()),
        api_token: raw.api_token,
        timeout: raw.timeout.map_or(15.0, |value| value.0 as f64),
    })
}
