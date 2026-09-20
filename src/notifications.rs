use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Url};

use crate::{
    config::PushoverConfig,
    models::{Delivery, Notification, Severity},
};

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Maximum simultaneous requests; the delivery service reserves capacity before
    /// spending an attempt or starting the request deadline.
    fn max_concurrency(&self) -> usize {
        4
    }
    async fn send(&self, title: &str, message: &str) -> Notification;
    async fn send_delivery(&self, delivery: &Delivery) -> Notification {
        self.send(&delivery.title, &delivery.message).await
    }
}

pub struct Pushover {
    config: PushoverConfig,
    client: Client,
    endpoint: Url,
    timeout: Duration,
}

impl Pushover {
    pub fn new(config: PushoverConfig) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(10))
                .build()?,
            endpoint: Url::parse("https://api.pushover.net/1/messages.json")?,
            timeout: Duration::from_secs(10),
        })
    }

    async fn attempt(&self, title: &str, message: &str, severity: Severity) -> Notification {
        let mut form = vec![
            ("token", self.config.app_token.expose()),
            ("user", self.config.user_key.expose()),
            ("title", title),
            ("message", message),
            (
                "priority",
                match severity {
                    Severity::Info => "-1",
                    Severity::Warning => "0",
                    Severity::Critical => "1",
                },
            ),
        ];
        if let Some(device) = &self.config.device {
            form.push(("device", device));
        }
        let response = match self
            .client
            .post(self.endpoint.clone())
            .form(&form)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return Notification::failed("Could not contact Pushover; delivery is uncertain.");
            }
        };
        if response.status() != reqwest::StatusCode::OK {
            return Notification::failed("Pushover rejected the notification.");
        }
        match response.json::<serde_json::Value>().await {
            Ok(body) if body.get("status").and_then(|s| s.as_i64()) == Some(1) => {
                Notification::sent()
            }
            Ok(_) => Notification::failed("Pushover did not accept the notification."),
            Err(_) => Notification::failed("Invalid Pushover response; delivery is uncertain."),
        }
    }
}

#[async_trait]
impl Notifier for Pushover {
    fn max_concurrency(&self) -> usize {
        2
    }
    async fn send(&self, title: &str, message: &str) -> Notification {
        // Capacity is reserved by the delivery service before this deadline starts.
        tokio::time::timeout(
            self.timeout,
            self.attempt(title, message, Severity::Warning),
        )
        .await
        .unwrap_or_else(|_| Notification::failed("Pushover timed out; delivery is uncertain."))
    }
    async fn send_delivery(&self, delivery: &Delivery) -> Notification {
        tokio::time::timeout(
            self.timeout,
            self.attempt(&delivery.title, &delivery.message, delivery.severity),
        )
        .await
        .unwrap_or_else(|_| Notification::failed("Pushover timed out; delivery is uncertain."))
    }
}

pub struct Webhook {
    url: crate::config::Secret,
    bearer_token: Option<crate::config::Secret>,
    client: Client,
}
impl Webhook {
    pub fn new(
        url: crate::config::Secret,
        bearer_token: Option<crate::config::Secret>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            url,
            bearer_token,
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
        })
    }
    async fn post(&self, body: serde_json::Value, id: Option<i64>) -> Notification {
        let mut request = self.client.post(self.url.expose()).json(&body);
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token.expose());
        }
        // Preserve the original wire prefix so queued retries retain their deduplication key.
        if let Some(id) = id {
            request = request.header("Idempotency-Key", format!("flare-delivery-{id}"));
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => Notification::sent(),
            Ok(_) => Notification::failed("Webhook rejected the notification."),
            Err(_) => Notification::failed("Could not contact webhook; delivery is uncertain."),
        }
    }
}
#[async_trait]
impl Notifier for Webhook {
    async fn send(&self, title: &str, message: &str) -> Notification {
        self.post(serde_json::json!({"title":title,"message":message}), None)
            .await
    }
    async fn send_delivery(&self, delivery: &Delivery) -> Notification {
        self.post(
            serde_json::json!({"id":delivery.id,"title":delivery.title,"message":delivery.message,
            "severity":delivery.severity,"kind":delivery.kind,"count":delivery.count}),
            Some(delivery.id),
        )
        .await
    }
}

#[cfg(test)]
mod webhook_tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn webhook_sends_metadata_and_stable_key_without_leaking_error_bodies() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let app = Router::new()
            .route(
                "/hook",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    captured.lock().unwrap().push((headers, body));
                    async { StatusCode::NO_CONTENT }
                }),
            )
            .route(
                "/fail",
                post(|| async { (StatusCode::BAD_REQUEST, "secret-provider-body") }),
            )
            .route(
                "/redirect",
                post(|| async { (StatusCode::TEMPORARY_REDIRECT, [("location", "/hook")]) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let channel = Webhook::new(
            serde_json::from_value(json!(format!("http://{address}/hook"))).unwrap(),
            Some(serde_json::from_value(json!("secret-token")).unwrap()),
        )
        .unwrap();
        let event = Delivery {
            id: 42,
            title: "Backup".into(),
            message: "Done 🦀".into(),
            severity: Severity::Critical,
            kind: "alert".into(),
            count: 1,
            created_at: 0,
            next_attempt_at: 0,
            notification: Notification::not_attempted(),
            destinations: vec![],
        };
        for _ in 0..2 {
            assert_eq!(
                channel.send_delivery(&event).await.status,
                crate::models::NotificationStatus::Sent
            );
        }
        {
            let requests = seen.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for (headers, body) in requests.iter() {
                assert_eq!(headers["authorization"], "Bearer secret-token");
                assert_eq!(headers["idempotency-key"], "flare-delivery-42");
                assert_eq!(
                    *body,
                    json!({"id":42,"title":"Backup","message":"Done 🦀","severity":"critical","kind":"alert","count":1})
                );
            }
        }
        for path in ["fail", "redirect"] {
            let channel = Webhook::new(
                serde_json::from_value(json!(format!("http://{address}/{path}"))).unwrap(),
                None,
            )
            .unwrap();
            let outcome = channel.send_delivery(&event).await;
            assert_eq!(outcome.status, crate::models::NotificationStatus::Failed);
            assert!(!outcome.error.unwrap().contains("secret"));
        }
        assert_eq!(seen.lock().unwrap().len(), 2);
        server.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::Form, http::StatusCode, response::IntoResponse, routing::post};
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    async fn check(status: StatusCode, body: &'static str, delay: Duration) -> Notification {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let router = Router::new().route(
            "/",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(form["token"], "a".repeat(30));
                    assert_eq!(form["user"], "u".repeat(30));
                    assert_eq!(form["title"], "Disk full");
                    assert_eq!(form["message"], "Free space < 5% & shrinking");
                    assert_eq!(form["priority"], "0");
                    assert_eq!(form["device"], "phone");
                    tokio::time::sleep(delay).await;
                    (status, body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = serde_yaml_ng::from_str(&format!(
            "app_token: {}\nuser_key: {}\ndevice: phone\n",
            "a".repeat(30),
            "u".repeat(30)
        ))
        .unwrap();
        let mut channel = Pushover::new(config).unwrap();
        channel.endpoint = Url::parse(&format!("http://{address}/")).unwrap();
        channel.timeout = Duration::from_millis(150);
        let result = channel
            .send("Disk full", "Free space < 5% & shrinking")
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
        result
    }

    #[tokio::test]
    async fn accepts_only_http_200_and_provider_success() {
        use crate::models::NotificationStatus::*;
        assert_eq!(
            check(StatusCode::OK, r#"{"status":1}"#, Duration::ZERO)
                .await
                .status,
            Sent
        );
        for (status, body) in [
            (StatusCode::OK, r#"{"status":0,"errors":["secret"]}"#),
            (StatusCode::OK, "not json"),
            (StatusCode::OK, "[]"),
            (StatusCode::TOO_MANY_REQUESTS, r#"{"status":1}"#),
            (StatusCode::INTERNAL_SERVER_ERROR, "secret"),
            (StatusCode::TEMPORARY_REDIRECT, "secret"),
        ] {
            let result = check(status, body, Duration::ZERO).await;
            assert_eq!(result.status, Failed);
            assert!(!result.error.unwrap().contains("secret"));
        }
    }

    #[tokio::test]
    async fn timeout_is_one_attempt_with_uncertain_delivery() {
        let result = check(StatusCode::OK, r#"{"status":1}"#, Duration::from_secs(1)).await;
        assert_eq!(result.status, crate::models::NotificationStatus::Failed);
        assert!(result.error.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn slow_pushover_deliveries_wait_without_spending_attempts_or_request_time() {
        use crate::{
            delivery::DeliveryService,
            models::{Alert, NotificationStatus},
            store::Store,
        };
        use tokio::sync::Semaphore;

        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/",
            post({
                let (started, release, active, peak, calls) = (
                    started.clone(),
                    release.clone(),
                    active.clone(),
                    peak.clone(),
                    calls.clone(),
                );
                move || {
                    let (started, release, active, peak, calls) = (
                        started.clone(),
                        release.clone(),
                        active.clone(),
                        peak.clone(),
                        calls.clone(),
                    );
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(concurrent, Ordering::SeqCst);
                        started.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({"status": 1}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = serde_yaml_ng::from_str(&format!(
            "app_token: {}\nuser_key: {}\n",
            "a".repeat(30),
            "u".repeat(30)
        ))
        .unwrap();
        let mut pushover = Pushover::new(config).unwrap();
        pushover.endpoint = Url::parse(&format!("http://{address}/")).unwrap();
        // Each request fits its deadline; two waves do not fit one shared deadline.
        pushover.timeout = Duration::from_secs(1);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
        let service = DeliveryService::new(store, Some(Arc::new(pushover)));
        assert_eq!(service.settings.max_attempts, 1);
        let mut tasks = tokio::task::JoinSet::new();
        for n in 0..4 {
            let service = service.clone();
            tasks.spawn(async move {
                service
                    .alert(
                        Alert {
                            title: format!("Alert {n}"),
                            message: "slow".into(),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .unwrap()
            });
        }
        tokio::time::timeout(Duration::from_secs(5), started.acquire_many(2))
            .await
            .unwrap()
            .unwrap()
            .forget();
        let jobs = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let jobs: Vec<Delivery> = service
                    .store
                    .run(|db| {
                        db.prepare("SELECT payload FROM deliveries")?
                            .query_map([], |row| row.get::<_, String>(0))?
                            .map(|value| Ok(serde_json::from_str(&value?).unwrap()))
                            .collect()
                    })
                    .await
                    .unwrap();
                if jobs.len() == 4 {
                    break jobs;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            jobs.iter()
                .map(|job| job.destinations[0].attempts)
                .sum::<u32>(),
            2
        );
        assert_eq!(
            jobs.iter()
                .filter(|job| job.destinations[0].attempts == 0)
                .count(),
            2
        );
        assert!(
            jobs.iter()
                .all(|job| job.notification.status == NotificationStatus::Pending)
        );
        release.add_permits(4);
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                let job = result.unwrap();
                assert_eq!(job.notification.status, NotificationStatus::Sent);
                assert_eq!(job.destinations[0].attempts, 1);
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
