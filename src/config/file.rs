//! Strict YAML schemas for the structured configuration format.
use super::*;
use flares_client::config::{Seconds, read};
use std::net::SocketAddr;

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
