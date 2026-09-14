use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Url};
use tokio::sync::Semaphore;

use crate::{config::PushoverConfig, models::Notification};

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send(&self, title: &str, message: &str) -> Notification;
}

pub struct Pushover {
    config: PushoverConfig,
    client: Client,
    permits: Semaphore,
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
            permits: Semaphore::new(2),
            endpoint: Url::parse("https://api.pushover.net/1/messages.json")?,
            timeout: Duration::from_secs(10),
        })
    }

    async fn attempt(&self, title: &str, message: &str) -> Notification {
        let Ok(_permit) = self.permits.acquire().await else {
            return Notification::failed("Notification channel is unavailable.");
        };
        let mut form = vec![
            ("token", self.config.app_token.expose()),
            ("user", self.config.user_key.expose()),
            ("title", title),
            ("message", message),
            ("priority", "0"),
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
    async fn send(&self, title: &str, message: &str) -> Notification {
        // The deadline also covers capacity waiting and reading the response body.
        tokio::time::timeout(self.timeout, self.attempt(title, message))
            .await
            .unwrap_or_else(|_| Notification::failed("Pushover timed out; delivery is uncertain."))
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
}
