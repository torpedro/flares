use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use flare::{api, models::*, notifications::Notifier, store::Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tower::ServiceExt;

struct CountingNotifier {
    calls: AtomicUsize,
    fail: bool,
    messages: Mutex<Vec<(String, String)>>,
}
#[async_trait]
impl Notifier for CountingNotifier {
    async fn send(&self, title: &str, message: &str) -> Notification {
        self.messages
            .lock()
            .unwrap()
            .push((title.into(), message.into()));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Notification::failed("Provider unavailable")
        } else {
            Notification::sent()
        }
    }
}

struct Harness {
    _dir: TempDir,
    store: Store,
    app: Router,
    notifier: Arc<CountingNotifier>,
}
impl Harness {
    fn new(fail: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_file(&dir.path().join("issues.sqlite3")).unwrap();
        let notifier = Arc::new(CountingNotifier {
            calls: AtomicUsize::new(0),
            fail,
            messages: Mutex::new(Vec::new()),
        });
        let app = api::router(
            store.clone(),
            Some(notifier.clone()),
            serde_json::from_value(json!("test-token")).unwrap(),
        );
        Self {
            _dir: dir,
            store,
            app,
            notifier,
        }
    }
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("authorization", auth);
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(body.map_or(Body::empty(), |value| Body::from(value.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, body) = request(app, method, path, body, Some("Bearer test-token")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn alerts_send_every_time_without_creating_or_changing_issues() {
    for fail in [false, true] {
        let h = Harness::new(fail);
        let issue = call(
            &h.app,
            "POST",
            "/v1/issues/open",
            Some(json!({"id":"backup"})),
        )
        .await;
        for _ in 0..2 {
            let result = call(
                &h.app,
                "POST",
                "/v1/alerts",
                Some(json!({
                    "title":"backup", "message":"Backup completed 🦀"
                })),
            )
            .await;
            let expected = if fail {
                Notification::failed("Provider unavailable")
            } else {
                Notification::sent()
            };
            assert_eq!(result["notification"], json!(expected));
            assert!(result["delivery_id"].is_i64());
        }
        assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            &h.notifier.messages.lock().unwrap()[1..],
            &[
                ("backup".into(), "Backup completed 🦀".into()),
                ("backup".into(), "Backup completed 🦀".into()),
            ]
        );
        assert_eq!(call(&h.app, "GET", "/v1/issues", None).await["total"], 1);
        assert_eq!(
            call(&h.app, "GET", "/v1/issues/backup", None).await,
            issue["issue"]
        );
        assert_eq!(
            request(
                &h.app,
                "POST",
                "/v1/alerts/close",
                Some(json!({"title":"backup"})),
                Some("Bearer test-token")
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn alerts_validate_content_before_sending() {
    let h = Harness::new(false);
    for body in [
        json!({}),
        json!({"title":"Alert"}),
        json!({"message":"Hello"}),
        json!({"title":null,"message":"Hello"}),
        json!({"title":"Alert","message":42}),
        json!({"title":"Alert","message":"Hello","id":"x"}),
        json!({"title":"","message":"Hello"}),
        json!({"title":"🦀".repeat(251),"message":"Hello"}),
        json!({"title":"Alert","message":""}),
        json!({"title":"Alert","message":"🦀".repeat(1025)}),
    ] {
        assert_eq!(
            request(
                &h.app,
                "POST",
                "/v1/alerts",
                Some(body),
                Some("Bearer test-token")
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 0);
    call(
        &h.app,
        "POST",
        "/v1/alerts",
        Some(json!({"title":"🦀".repeat(250),"message":"🦀".repeat(1024)})),
    )
    .await;
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 1);
    assert_eq!(call(&h.app, "GET", "/v1/issues", None).await["total"], 0);
}

#[tokio::test]
async fn alerts_without_a_notifier_report_not_attempted() {
    let h = Harness::new(false);
    let app = api::router(
        h.store.clone(),
        None,
        serde_json::from_value(json!("test-token")).unwrap(),
    );
    let result = call(
        &app,
        "POST",
        "/v1/alerts",
        Some(json!({"title":"Alert","message":"Hello"})),
    )
    .await;
    assert_eq!(
        result["notification"],
        json!({"status":"not_attempted","error":null})
    );
    assert_eq!(call(&app, "GET", "/v1/issues", None).await["total"], 0);
}

#[tokio::test]
async fn cancelled_alert_request_does_not_cancel_notification() {
    struct BlockingNotifier {
        started: Semaphore,
        release: Semaphore,
        completed: Semaphore,
    }
    #[async_trait]
    impl Notifier for BlockingNotifier {
        async fn send(&self, _title: &str, _message: &str) -> Notification {
            self.started.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            self.completed.add_permits(1);
            Notification::sent()
        }
    }
    let h = Harness::new(false);
    let notifier = Arc::new(BlockingNotifier {
        started: Semaphore::new(0),
        release: Semaphore::new(0),
        completed: Semaphore::new(0),
    });
    let app = api::router(
        h.store.clone(),
        Some(notifier.clone()),
        serde_json::from_value(json!("test-token")).unwrap(),
    );
    let pending = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/v1/alerts",
            Some(json!({"title":"Alert","message":"Hello"})),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        notifier.started.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    notifier.release.add_permits(1);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        notifier.completed.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    assert_eq!(call(&h.app, "GET", "/v1/issues", None).await["total"], 0);
}

#[tokio::test]
async fn lifecycle_defaults_duplicates_content_and_timestamps() {
    let h = Harness::new(false);
    let opened = call(
        &h.app,
        "POST",
        "/v1/issues/open",
        Some(json!({"id":"disk"})),
    )
    .await;
    assert_eq!(opened["changed"], true);
    assert_eq!(opened["notification"]["status"], "sent");
    assert_eq!(opened["issue"]["title"], "disk");
    assert_eq!(opened["issue"]["message"], "Issue disk opened.");
    assert_eq!(opened["issue"]["opening_count"], 1);
    assert_eq!(opened["issue"]["closed_at"], Value::Null);
    let duplicate = call(
        &h.app,
        "POST",
        "/v1/issues/open",
        Some(json!({"id":"disk","title":"ignored","message":"ignored"})),
    )
    .await;
    assert_eq!(duplicate["issue"], opened["issue"]);
    assert_eq!(duplicate["changed"], false);
    assert_eq!(duplicate["notification"]["status"], "not_attempted");
    let closed = call(
        &h.app,
        "POST",
        "/v1/issues/close",
        Some(json!({"id":"disk"})),
    )
    .await;
    assert_eq!(closed["issue"]["status"], "closed");
    assert!(closed["issue"]["closed_at"].is_string());
    assert_eq!(closed["notification"]["status"], "not_attempted");
    let closed_again = call(
        &h.app,
        "POST",
        "/v1/issues/close",
        Some(json!({"id":"disk"})),
    )
    .await;
    assert_eq!(closed_again["changed"], false);
    assert_eq!(closed_again["issue"], closed["issue"]);
    let reopened = call(
        &h.app,
        "POST",
        "/v1/issues/open",
        Some(json!({"id":"disk","title":"Disk full again","message":"Free space low"})),
    )
    .await;
    assert_eq!(reopened["issue"]["opening_count"], 2);
    assert_eq!(reopened["issue"]["title"], "Disk full again");
    assert_eq!(reopened["issue"]["message"], "Free space low");
    assert_eq!(
        reopened["issue"]["created_at"],
        opened["issue"]["created_at"]
    );
    assert_eq!(reopened["issue"]["closed_at"], closed["issue"]["closed_at"]);
    call(
        &h.app,
        "POST",
        "/v1/issues/close",
        Some(json!({"id":"disk"})),
    )
    .await;
    let defaulted = call(
        &h.app,
        "POST",
        "/v1/issues/open",
        Some(json!({"id":"disk"})),
    )
    .await;
    assert_eq!(defaulted["issue"]["title"], "disk");
    assert_eq!(defaulted["issue"]["message"], "Issue disk reopened.");
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failed_notification_keeps_issue_open_and_is_not_retried() {
    let h = Harness::new(true);
    let opened = call(&h.app, "POST", "/v1/issues/open", Some(json!({"id":"x"}))).await;
    assert_eq!(opened["notification"]["status"], "failed");
    assert_eq!(opened["issue"]["status"], "open");
    assert_eq!(
        call(&h.app, "GET", "/v1/issues/x", None).await,
        opened["issue"]
    );
    let duplicate = call(&h.app, "POST", "/v1/issues/open", Some(json!({"id":"x"}))).await;
    assert_eq!(duplicate["notification"]["status"], "not_attempted");
    assert_eq!(duplicate["issue"]["notification"]["status"], "failed");
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auth_validation_and_unknown_ids_do_not_mutate() {
    let h = Harness::new(false);
    for (method, path, body) in [
        ("GET", "/metrics", None),
        ("GET", "/v1/deliveries/1", None),
        ("GET", "/v1/heartbeats", None),
        ("POST", "/v1/heartbeats", Some(json!({}))),
        ("POST", "/v1/heartbeats/check-in", Some(json!({"id":"x"}))),
        ("DELETE", "/v1/heartbeat?id=x", None),
        (
            "POST",
            "/v1/alerts",
            Some(json!({"title":"Alert","message":"Hello"})),
        ),
        ("POST", "/v1/issues/open", Some(json!({"id":"x"}))),
        ("POST", "/v1/issues/close", Some(json!({"id":"x"}))),
        ("GET", "/v1/issues/x", None),
        ("GET", "/v1/issues", None),
        ("GET", "/v1/issue?id=x", None),
    ] {
        for auth in [None, Some("Bearer wrong"), Some("Basic test-token")] {
            assert_eq!(
                request(&h.app, method, path, body.clone(), auth).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
    }
    for body in [
        json!({}),
        json!({"id":""}),
        json!({"id":3}),
        json!({"id":"x","extra":1}),
        json!({"id":"x".repeat(201)}),
        json!({"id":"x","title":""}),
        json!({"id":"x","title":"a".repeat(251)}),
        json!({"id":"x","message":""}),
        json!({"id":"x","message":"a".repeat(1025)}),
    ] {
        assert_eq!(
            request(
                &h.app,
                "POST",
                "/v1/issues/open",
                Some(body),
                Some("Bearer test-token")
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        request(
            &h.app,
            "POST",
            "/v1/issues/close",
            Some(json!({"id":"missing"})),
            Some("Bearer test-token")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &h.app,
            "GET",
            "/v1/issues/missing",
            None,
            Some("Bearer test-token")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for suffix in [
        "?status=invalid",
        "?limit=0",
        "?limit=1001",
        "?offset=-1",
        "?limit=abc",
    ] {
        assert_eq!(
            request(
                &h.app,
                "GET",
                &format!("/v1/issues{suffix}"),
                None,
                Some("Bearer test-token")
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(call(&h.app, "GET", "/v1/issues", None).await["total"], 0);
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn opaque_ids_unicode_limits_and_listing() {
    let h = Harness::new(false);
    for id in ["open", "close", "a/b ?#%", ".", "..", "Case", "case"] {
        call(&h.app, "POST", "/v1/issues/open", Some(json!({"id":id}))).await;
        let mut url = reqwest::Url::parse("http://localhost/v1/issue").unwrap();
        url.query_pairs_mut().append_pair("id", id);
        assert_eq!(
            call(
                &h.app,
                "GET",
                &format!("{}?{}", url.path(), url.query().unwrap()),
                None
            )
            .await["id"],
            id
        );
    }
    for id in ["open", "close"] {
        assert_eq!(
            call(&h.app, "GET", &format!("/v1/issues/{id}"), None).await["id"],
            id
        );
    }
    assert_eq!(
        call(&h.app, "GET", "/v1/issues/a%2Fb%20%3F%23%25", None).await["id"],
        "a/b ?#%"
    );
    call(
        &h.app,
        "POST",
        "/v1/issues/open",
        Some(json!({"id":"🦀".repeat(200),"title":"🦀".repeat(250),"message":"🦀".repeat(1024)})),
    )
    .await;
    call(
        &h.app,
        "POST",
        "/v1/issues/close",
        Some(json!({"id":"Case"})),
    )
    .await;
    let listed = call(&h.app, "GET", "/v1/issues?limit=2&offset=0", None).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 2);
    assert_eq!(listed["items"][0]["id"], "Case");
    assert_eq!(listed["total"], 8);
    let closed = call(&h.app, "GET", "/v1/issues?status=closed", None).await;
    assert_eq!(closed["total"], 1);
    assert_eq!(closed["items"][0]["id"], "Case");
    assert_eq!(
        call(&h.app, "GET", "/v1/issues?status=open", None).await["total"],
        7
    );
    assert_eq!(
        call(&h.app, "GET", "/v1/issues?offset=100", None).await["items"],
        json!([])
    );
}

struct DelayedNotifier {
    started: Semaphore,
    release: Semaphore,
    calls: AtomicUsize,
}
#[async_trait]
impl Notifier for DelayedNotifier {
    async fn send(&self, title: &str, _message: &str) -> Notification {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if title == "slow" {
            self.started.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Notification::failed("Late first outcome")
        } else {
            Notification::sent()
        }
    }
}

#[tokio::test]
async fn concurrent_opens_and_late_results_are_bound_to_their_opening() {
    let h = Harness::new(false);
    let notifier = Arc::new(DelayedNotifier {
        started: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let app = api::router(
        h.store.clone(),
        Some(notifier.clone()),
        serde_json::from_value(json!("test-token")).unwrap(),
    );
    let first_app = app.clone();
    let first = tokio::spawn(async move {
        call(
            &first_app,
            "POST",
            "/v1/issues/open",
            Some(json!({"id":"x","title":"slow"})),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        notifier.started.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    let mut duplicates = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let app = app.clone();
        duplicates.spawn(async move {
            call(&app, "POST", "/v1/issues/open", Some(json!({"id":"x"}))).await
        });
    }
    while let Some(result) = duplicates.join_next().await {
        let result = result.unwrap();
        assert_eq!(result["changed"], false);
        assert_eq!(result["issue"]["notification"]["status"], "pending");
    }
    call(&app, "POST", "/v1/issues/close", Some(json!({"id":"x"}))).await;
    let reopened = call(&app, "POST", "/v1/issues/open", Some(json!({"id":"x"}))).await;
    assert_eq!(reopened["issue"]["opening_count"], 2);
    notifier.release.add_permits(1);
    assert_eq!(first.await.unwrap()["notification"]["status"], "failed");
    assert_eq!(
        call(&app, "GET", "/v1/issues/x", None).await,
        reopened["issue"]
    );
    assert_eq!(notifier.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn restart_preserves_issues_and_marks_unfinished_attempt_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite3");
    let store = Store::open_file(&path).unwrap();
    for id in ["sent", "pending", "closed"] {
        store
            .open(
                OpenIssue {
                    id: id.into(),
                    title: None,
                    message: None,
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
    }
    store
        .record_notification("sent".into(), 1, Notification::sent())
        .await
        .unwrap();
    store.close("closed".into()).await.unwrap();
    drop(store);
    let store = Store::open_file(&path).unwrap();
    assert_eq!(
        store.get("sent".into()).await.unwrap().notification.status,
        NotificationStatus::Sent
    );
    let pending = store.get("pending".into()).await.unwrap();
    assert_eq!(pending.notification.status, NotificationStatus::Unknown);
    assert_eq!(pending.status, IssueStatus::Open);
    assert_eq!(
        store.get("closed".into()).await.unwrap().status,
        IssueStatus::Closed
    );
    let (_, changed) = store
        .open(
            OpenIssue {
                id: "pending".into(),
                title: None,
                message: None,
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert!(!changed);
}

#[tokio::test]
async fn simultaneous_first_opens_create_exactly_one_opening() {
    let h = Harness::new(false);
    let barrier = Arc::new(tokio::sync::Barrier::new(20));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let app = h.app.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            call(&app, "POST", "/v1/issues/open", Some(json!({"id":"race"}))).await
        });
    }
    let mut changes = 0;
    while let Some(value) = tasks.join_next().await {
        let value = value.unwrap();
        changes += usize::from(value["changed"] == true);
        assert_eq!(value["issue"]["opening_count"], 1);
    }
    assert_eq!(changes, 1);
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_http_request_does_not_cancel_committed_notification() {
    let h = Harness::new(false);
    let notifier = Arc::new(DelayedNotifier {
        started: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let app = api::router(
        h.store.clone(),
        Some(notifier.clone()),
        serde_json::from_value(json!("test-token")).unwrap(),
    );
    let pending = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/v1/issues/open",
            Some(json!({"id":"cancelled","title":"slow"})),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        notifier.started.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    notifier.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let issue = h.store.get("cancelled".into()).await.unwrap();
            if issue.notification.status != NotificationStatus::Pending {
                assert_eq!(issue.notification.status, NotificationStatus::Failed);
                assert_eq!(issue.status, IssueStatus::Open);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(notifier.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn openapi_describes_authenticated_operations() {
    let h = Harness::new(false);
    let (status, doc) = request(&h.app, "GET", "/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        doc["components"]["securitySchemes"]["bearer_token"]["scheme"],
        "bearer"
    );
    for (path, method) in [
        ("/v1/deliveries/{id}", "get"),
        ("/v1/heartbeats", "get"),
        ("/v1/heartbeats", "post"),
        ("/v1/heartbeats/check-in", "post"),
        ("/v1/heartbeat", "delete"),
        ("/v1/alerts", "post"),
        ("/v1/issues/open", "post"),
        ("/v1/issues/close", "post"),
        ("/v1/issues/{id}", "get"),
        ("/v1/issue", "get"),
        ("/v1/issues", "get"),
    ] {
        assert_eq!(
            doc["paths"][path][method]["security"][0],
            json!({"bearer_token":[]})
        );
    }
}

#[tokio::test]
async fn alert_idempotency_header_and_delivery_lookup() {
    let h = Harness::new(false);
    async fn keyed(app: &Router, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/alerts")
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .header("Idempotency-Key", "job-123")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        (
            response.status(),
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap(),
        )
    }
    let body = json!({"title":"Backup","message":"Complete","severity":"info"});
    let (status, first) = keyed(&h.app, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keyed(&h.app, body).await.1, first);
    assert_eq!(
        keyed(&h.app, json!({"title":"Backup","message":"Different"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let delivery = call(
        &h.app,
        "GET",
        &format!("/v1/deliveries/{}", first["delivery_id"]),
        None,
    )
    .await;
    assert_eq!(delivery["severity"], "info");
    assert_eq!(delivery["destinations"][0]["attempts"], 1);
    assert_eq!(h.notifier.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn health_readiness_metrics_and_heartbeat_endpoints() {
    let h = Harness::new(false);
    for path in ["/healthz", "/readyz"] {
        assert_eq!(
            request(&h.app, "GET", path, None, None).await.0,
            StatusCode::OK
        );
    }
    for body in [
        json!({"id":"x","title":"Job","interval_seconds":0}),
        json!({"id":"x","title":"Job","interval_seconds":10,"severity":"invalid"}),
    ] {
        assert_eq!(
            request(
                &h.app,
                "POST",
                "/v1/heartbeats",
                Some(body),
                Some("Bearer test-token")
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let beat=call(&h.app,"POST","/v1/heartbeats",Some(json!({"id":"a/../b","title":"Backup","interval_seconds":60,"grace_seconds":10,"notify_on_recovery":true}))).await;
    assert_eq!(
        beat["due_at"].as_i64().unwrap() - beat["last_seen"].as_i64().unwrap(),
        70
    );
    assert_eq!(
        call(&h.app, "GET", "/v1/heartbeats", None)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/heartbeats/check-in",
            Some(json!({"id":"a/../b"}))
        )
        .await["overdue"],
        false
    );
    call(&h.app, "DELETE", "/v1/heartbeat?id=a%2F..%2Fb", None).await;
    assert_eq!(
        request(
            &h.app,
            "POST",
            "/v1/heartbeats/check-in",
            Some(json!({"id":"a/../b"})),
            Some("Bearer test-token")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let metrics = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert!(
        metrics.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let body = String::from_utf8(
        to_bytes(metrics.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("flare_notification_attempts_total 0"));
    // Liveness survives a database failure; readiness reports it.
    rusqlite::Connection::open(h._dir.path().join("issues.sqlite3"))
        .unwrap()
        .execute("DROP TABLE delivery_metrics", [])
        .unwrap();
    assert_eq!(
        request(&h.app, "GET", "/readyz", None, None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        request(&h.app, "GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
}
