use super::*;
use async_trait::async_trait;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Fake {
    failures: AtomicUsize,
    calls: Mutex<Vec<(String, String)>>,
}
#[async_trait]
impl Notifier for Fake {
    async fn send(&self, title: &str, message: &str) -> Notification {
        self.calls
            .lock()
            .unwrap()
            .push((title.into(), message.into()));
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            Notification::failed("Provider unavailable")
        } else {
            Notification::sent()
        }
    }
}
fn setup() -> (tempfile::TempDir, DeliveryService, Arc<Fake>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
    let channel = Arc::new(Fake::default());
    let service = DeliveryService::new(store, Some(channel.clone()));
    (dir, service, channel)
}
fn alert() -> Alert {
    Alert {
        title: "Backup".into(),
        message: "Completed".into(),
        ..Default::default()
    }
}
async fn due(service: &DeliveryService, id: i64) {
    service
        .store
        .run(move |db| {
            let mut job = load(db, id)?;
            job.next_attempt_at = 0;
            save(db, &job, "pending")
        })
        .await
        .unwrap();
}
async fn jobs(service: &DeliveryService) -> Vec<Delivery> {
    service
        .store
        .run(|db| {
            db.prepare("SELECT payload FROM deliveries ORDER BY id")?
                .query_map([], |r| r.get::<_, String>(0))?
                .map(|r| decode(r?))
                .collect()
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn idempotency_is_atomic_and_survives_restart() {
    let (dir, service, channel) = setup();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let service = service.clone();
        tasks.spawn(async move {
            service
                .alert(alert(), Some("backup-123".into()))
                .await
                .unwrap()
                .id
        });
    }
    let mut ids = Vec::new();
    while let Some(id) = tasks.join_next().await {
        ids.push(id.unwrap());
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
    let changed = Alert {
        message: "Different".into(),
        ..alert()
    };
    assert!(matches!(
        service.alert(changed, Some("backup-123".into())).await,
        Err(StoreError::Conflict)
    ));
    drop(service);
    let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
    let service = DeliveryService::new(store, Some(channel.clone()));
    let repeated = service
        .alert(alert(), Some("backup-123".into()))
        .await
        .unwrap();
    assert_eq!(repeated.id, ids[0]);
    assert_eq!(repeated.notification.status, NotificationStatus::Sent);
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
    assert_ne!(service.alert(alert(), None).await.unwrap().id, ids[0]);
}

#[tokio::test]
async fn retries_are_bounded_and_do_not_resend_successful_destinations() {
    let (_dir, mut service, first) = setup();
    let second = Arc::new(Fake::default());
    second.failures.store(10, Ordering::SeqCst);
    service.channels.insert("second".into(), second.clone());
    service.defaults.push("second".into());
    service.settings.max_attempts = 3;
    service.settings.retry_base_seconds = 2;
    service.settings.retry_max_seconds = 3;
    let job = service.alert(alert(), None).await.unwrap();
    assert_eq!(job.notification.status, NotificationStatus::Pending);
    assert!(job.next_attempt_at >= job.created_at + 2);
    assert_eq!(
        service.process(job.id).await.unwrap().destinations[1].attempts,
        1
    );
    due(&service, job.id).await;
    let retry = service.process(job.id).await.unwrap();
    assert_eq!(retry.destinations[1].attempts, 2);
    assert!(retry.next_attempt_at >= Utc::now().timestamp() + 2);
    due(&service, job.id).await;
    let final_job = service.process(job.id).await.unwrap();
    assert_eq!(final_job.notification.status, NotificationStatus::Failed);
    service.process(job.id).await.unwrap();
    assert_eq!(first.calls.lock().unwrap().len(), 1);
    assert_eq!(second.calls.lock().unwrap().len(), 3);
    let metrics = service.metrics().await.unwrap();
    assert!(metrics.contains("flare_notification_attempts_total 4\n"));
    assert!(metrics.contains("flare_notifications_total{status=\"failed\"} 3\n"));
    assert!(metrics.contains("flare_delivery_queue_depth 0\n"));
}

#[tokio::test]
async fn restart_recovers_claimed_jobs_without_resending_recorded_success() {
    let (dir, mut service, channel) = setup();
    service.settings.max_attempts = 3;
    channel.failures.store(1, Ordering::SeqCst);
    let job = service.alert(alert(), None).await.unwrap();
    let id = job.id;
    service
        .store
        .run(move |db| {
            let mut job = load(db, id)?;
            job.next_attempt_at = 0;
            job.destinations[0].notification = Notification::sent();
            // Simulate crash after recording provider success, before finalizing the job.
            save(db, &job, "running")
        })
        .await
        .unwrap();
    drop(service);
    let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
    let mut service = DeliveryService::new(store, Some(channel.clone()));
    service.settings.max_attempts = 3;
    assert_eq!(
        service.process(id).await.unwrap().notification.status,
        NotificationStatus::Sent
    );
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
    let next = service.alert(alert(), None).await.unwrap();
    assert_eq!(next.notification.status, NotificationStatus::Sent);
}

#[tokio::test]
async fn grouping_summarizes_once_and_rate_limits_defer_without_spending_attempts() {
    let (_dir, mut service, channel) = setup();
    service.settings.rate_limit_per_minute = 1;
    let request = Alert {
        group_key: Some("backups".into()),
        ..alert()
    };
    let first = service
        .alert(request.clone(), Some("one".into()))
        .await
        .unwrap();
    let second = service
        .alert(
            Alert {
                message: "Latest backup".into(),
                ..request.clone()
            },
            Some("two".into()),
        )
        .await
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.count, 2);
    assert_eq!(
        service
            .alert(request, Some("one".into()))
            .await
            .unwrap()
            .count,
        2
    );
    assert!(channel.calls.lock().unwrap().is_empty());
    due(&service, first.id).await;
    assert_eq!(
        service.process(first.id).await.unwrap().notification.status,
        NotificationStatus::Sent
    );
    assert_eq!(
        channel.calls.lock().unwrap()[0].1,
        "2 alerts grouped. Latest: Latest backup"
    );
    let deferred = service.alert(alert(), None).await.unwrap();
    assert_eq!(deferred.notification.status, NotificationStatus::Pending);
    assert_eq!(deferred.destinations[0].attempts, 0);
    service
        .store
        .run(|db| {
            db.execute("UPDATE delivery_metrics SET minute_count=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    due(&service, deferred.id).await;
    assert_eq!(
        service
            .process(deferred.id)
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::Sent
    );
}

#[tokio::test]
async fn severity_routes_and_group_boundaries_are_respected() {
    let (_dir, mut service, default) = setup();
    let urgent = Arc::new(Fake::default());
    service.channels.insert("urgent".into(), urgent.clone());
    service
        .routes
        .insert(Severity::Critical, vec!["urgent".into()]);
    service.routes.insert(Severity::Info, vec![]);
    let critical = Alert {
        severity: Severity::Critical,
        ..alert()
    };
    service.alert(critical.clone(), None).await.unwrap();
    assert_eq!(default.calls.lock().unwrap().len(), 0);
    assert_eq!(urgent.calls.lock().unwrap().len(), 1);
    assert_eq!(
        service
            .alert(
                Alert {
                    severity: Severity::Info,
                    ..alert()
                },
                None
            )
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::NotAttempted
    );
    let one = service
        .alert(
            Alert {
                group_key: Some("x".into()),
                ..alert()
            },
            None,
        )
        .await
        .unwrap();
    let two = service
        .alert(
            Alert {
                group_key: Some("x".into()),
                ..critical
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(one.id, two.id);
}

#[tokio::test]
async fn reminders_stop_on_close_and_resolution_is_enqueued_once() {
    let (_dir, service, _channel) = setup();
    let request = OpenIssue {
        id: "disk".into(),
        remind_every_seconds: Some(60),
        notify_on_resolution: true,
        ..Default::default()
    };
    let (_, _, opening) = service
        .store
        .open_planned(request.clone(), true, Some(service.plan(request.severity)))
        .await
        .unwrap();
    assert!(opening.is_some());
    service
        .store
        .run(|db| {
            db.execute("UPDATE issue_policies SET next_reminder=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.schedule().await.unwrap();
    service.schedule().await.unwrap();
    let all = jobs(&service).await;
    assert_eq!(all.iter().filter(|job| job.kind == "reminder").count(), 1);
    let (_, changed, resolution) = service
        .store
        .close_planned("disk".into(), Some(service.clone()))
        .await
        .unwrap();
    assert!(changed && resolution.is_some());
    assert!(
        !service
            .store
            .close_planned("disk".into(), Some(service.clone()))
            .await
            .unwrap()
            .1
    );
    service
        .store
        .run(|db| {
            db.execute("UPDATE issue_policies SET next_reminder=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.schedule().await.unwrap();
    assert_eq!(jobs(&service).await.len(), 3);
    // A later retry of the opening must not change the issue's lifecycle.
    service.process(opening.unwrap()).await.unwrap();
    assert_eq!(
        service.store.get("disk".into()).await.unwrap().status,
        IssueStatus::Closed
    );
}

#[tokio::test]
async fn heartbeat_outages_and_recoveries_are_durable_and_one_per_transition() {
    let (dir, service, channel) = setup();
    let beat = service
        .register_heartbeat(HeartbeatInput {
            id: "nightly".into(),
            title: "Nightly backup".into(),
            interval_seconds: 60,
            grace_seconds: 10,
            severity: Severity::Critical,
            notify_on_recovery: true,
        })
        .await
        .unwrap();
    assert_eq!(beat.due_at - beat.last_seen, 70);
    service
        .store
        .run(|db| {
            db.execute("UPDATE heartbeats SET due=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.schedule().await.unwrap();
    assert!(service.heartbeats().await.unwrap()[0].overdue);
    drop(service);
    let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
    let service = DeliveryService::new(store, Some(channel));
    service.schedule().await.unwrap();
    assert_eq!(jobs(&service).await.len(), 1);
    assert!(!service.check_in("nightly".into()).await.unwrap().overdue);
    service.check_in("nightly".into()).await.unwrap();
    assert_eq!(
        jobs(&service)
            .await
            .iter()
            .map(|j| j.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["heartbeat_missed", "heartbeat_recovered"]
    );
    service.delete_heartbeat("nightly".into()).await.unwrap();
    assert!(matches!(
        service.check_in("nightly".into()).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn worker_delivers_due_jobs_and_exits_when_shutdown_channel_closes() {
    let (_dir, service, channel) = setup();
    let queued = service
        .alert(
            Alert {
                group_key: Some("x".into()),
                ..alert()
            },
            None,
        )
        .await
        .unwrap();
    due(&service, queued.id).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(service.clone().run(rx));
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.get(queued.id).await.unwrap().notification.status != NotificationStatus::Sent
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(stop);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn crash_after_reserving_last_attempt_reports_uncertainty_without_retrying() {
    let (dir, service, channel) = setup();
    let queued = service
        .alert(
            Alert {
                group_key: Some("delay".into()),
                ..alert()
            },
            None,
        )
        .await
        .unwrap();
    let id = queued.id;
    service
        .store
        .run(move |db| {
            let mut job = load(db, id)?;
            job.destinations[0].attempts = 1;
            job.next_attempt_at = 0;
            save(db, &job, "running")
        })
        .await
        .unwrap();
    drop(service);
    let service = DeliveryService::new(
        Store::open_file(&dir.path().join("db.sqlite3")).unwrap(),
        Some(channel.clone()),
    );
    let job = service.process(id).await.unwrap();
    assert_eq!(job.notification.status, NotificationStatus::Failed);
    assert_eq!(
        job.destinations[0].notification.status,
        NotificationStatus::Failed
    );
    assert!(job.notification.error.unwrap().contains("uncertain"));
    assert!(channel.calls.lock().unwrap().is_empty());
}
