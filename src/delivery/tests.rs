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
            db.execute(
                "UPDATE delivery_targets SET due=0 WHERE delivery_id=?",
                [id],
            )?;
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
    service
        .channels
        .insert("second".into(), Channel::new(second.clone()));
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
    service
        .channels
        .insert("urgent".into(), Channel::new(urgent.clone()));
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
async fn expired_group_stays_closed_after_rate_limit_deferral_and_restart() {
    let (dir, mut service, channel) = setup();
    service.settings.rate_limit_per_minute = 1;
    let request = Alert {
        group_key: Some("backups".into()),
        ..alert()
    };
    let original = service.alert(request.clone(), None).await.unwrap();
    let id = original.id;
    service
        .store
        .run(move |db| {
            // Simulate expiry without waiting on the wall clock, then exhaust this minute's budget.
            let now = Utc::now().timestamp();
            let mut job = load(db, id)?;
            job.created_at = now - 60;
            job.next_attempt_at = 0;
            db.execute(
                "UPDATE delivery_targets SET due=0 WHERE delivery_id=?",
                [id],
            )?;
            save(db, &job, "pending")?;
            db.execute(
                "UPDATE deliveries SET group_until=? WHERE id=?",
                params![now - 30, id],
            )?;
            db.execute(
                "UPDATE delivery_metrics SET minute=?,minute_count=1 WHERE id=1",
                [now / 60],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let deferred = service.process(id).await.unwrap();
    assert_eq!(deferred.notification.status, NotificationStatus::Pending);
    assert_eq!(deferred.destinations[0].attempts, 0);
    assert!(deferred.next_attempt_at > Utc::now().timestamp());
    drop(service);
    let mut service = DeliveryService::new(
        Store::open_file(&dir.path().join("db.sqlite3")).unwrap(),
        Some(channel.clone()),
    );
    // A changed configuration also must not reopen the expired window.
    service.settings.group_window_seconds = 600;
    let next = service
        .alert(
            Alert {
                message: "New window".into(),
                ..request.clone()
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(next.id, id);
    assert_eq!(service.get(id).await.unwrap().count, 1);
    assert_eq!(service.get(id).await.unwrap().message, original.message);
    let joined = service.alert(request, None).await.unwrap();
    assert_eq!(joined.id, next.id);
    assert_eq!(joined.count, 2);
    due(&service, id).await;
    assert_eq!(
        service.process(id).await.unwrap().notification.status,
        NotificationStatus::Sent
    );
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn grouping_migration_preserves_legacy_deliveries_and_idempotency_keys() {
    let (dir, service, channel) = setup();
    let request = Alert {
        group_key: Some("backups".into()),
        ..alert()
    };
    let original = service
        .alert(request.clone(), Some("legacy-key".into()))
        .await
        .unwrap();
    let path = dir.path().join("db.sqlite3");
    drop(service);
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP INDEX deliveries_group_window;
        ALTER TABLE deliveries DROP COLUMN group_until;
        CREATE INDEX deliveries_group ON deliveries(group_key, state, due);",
    )
    .unwrap();
    drop(db);
    let service = DeliveryService::new(Store::open_file(&path).unwrap(), Some(channel.clone()));
    let replay = service
        .alert(request.clone(), Some("legacy-key".into()))
        .await
        .unwrap();
    assert_eq!(replay.id, original.id);
    assert_eq!(replay.count, 1);
    let fresh = service.alert(request.clone(), None).await.unwrap();
    assert_ne!(fresh.id, original.id);
    drop(service);
    // Migration is idempotent and newly recorded windows survive another restart.
    let service = DeliveryService::new(Store::open_file(&path).unwrap(), Some(channel.clone()));
    let joined = service.alert(request, None).await.unwrap();
    assert_eq!(joined.id, fresh.id);
    assert_eq!(joined.count, 2);
    due(&service, original.id).await;
    assert_eq!(
        service
            .process(original.id)
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::Sent
    );
    assert_eq!(channel.calls.lock().unwrap().len(), 1);
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
            db.execute(
                "UPDATE delivery_targets SET due=0 WHERE delivery_id=?",
                [id],
            )?;
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

#[tokio::test]
async fn destination_retry_policies_keep_independent_deadlines_and_attempt_limits() {
    let (_dir, mut service, slow) = setup();
    let fast = Arc::new(Fake::default());
    slow.failures.store(10, Ordering::SeqCst);
    fast.failures.store(10, Ordering::SeqCst);
    service
        .channels
        .insert("fast".into(), Channel::new(fast.clone()));
    service.defaults.push("fast".into());
    service.policies.insert(
        "pushover".into(),
        DestinationPolicy {
            max_attempts: 2,
            retry_base_seconds: 30,
            retry_max_seconds: 30,
            rate_limit: None,
        },
    );
    service.policies.insert(
        "fast".into(),
        DestinationPolicy {
            max_attempts: 3,
            retry_base_seconds: 1,
            retry_max_seconds: 1,
            rate_limit: None,
        },
    );
    let first = service.alert(alert(), None).await.unwrap();
    assert_eq!(first.notification.status, NotificationStatus::Pending);
    let id = first.id;
    for expected in [2, 3] {
        service
            .store
            .run(move |db| {
                let mut job = load(db, id)?;
                job.next_attempt_at = 0;
                db.execute(
                    "UPDATE delivery_targets SET due=0 WHERE delivery_id=? AND destination='fast'",
                    [id],
                )?;
                save(db, &job, "pending")
            })
            .await
            .unwrap();
        let retry = service.process(id).await.unwrap();
        assert_eq!(retry.destinations[0].attempts, 1);
        assert_eq!(retry.destinations[1].attempts, expected);
    }
    due(&service, id).await;
    let final_job = service.process(id).await.unwrap();
    assert_eq!(final_job.notification.status, NotificationStatus::Failed);
    assert_eq!(slow.calls.lock().unwrap().len(), 2);
    assert_eq!(fast.calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn destination_rate_limit_survives_restart_and_does_not_block_other_destinations() {
    let (dir, mut service, limited) = setup();
    let other = Arc::new(Fake::default());
    service
        .channels
        .insert("other".into(), Channel::new(other.clone()));
    service.defaults.push("other".into());
    let policy = DestinationPolicy {
        max_attempts: 1,
        retry_base_seconds: 1,
        retry_max_seconds: 1,
        rate_limit: Some(crate::config::RateLimit {
            attempts: 1,
            window_seconds: 86400,
        }),
    };
    service.policies.insert("pushover".into(), policy.clone());
    assert_eq!(
        service
            .alert(alert(), None)
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::Sent
    );
    drop(service);
    let mut service = DeliveryService::new(
        Store::open_file(&dir.path().join("db.sqlite3")).unwrap(),
        Some(limited.clone()),
    );
    service
        .channels
        .insert("other".into(), Channel::new(other.clone()));
    service.defaults.push("other".into());
    service.policies.insert("pushover".into(), policy);
    let job = service.alert(alert(), None).await.unwrap();
    assert_eq!(job.notification.status, NotificationStatus::Pending);
    assert_eq!(job.destinations[0].attempts, 0);
    assert_eq!(
        job.destinations[1].notification.status,
        NotificationStatus::Sent
    );
    assert_eq!(limited.calls.lock().unwrap().len(), 1);
    assert_eq!(other.calls.lock().unwrap().len(), 2);
    service
        .store
        .run(|db| {
            db.execute("UPDATE destination_rate_limits SET count=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    due(&service, job.id).await;
    assert_eq!(
        service.process(job.id).await.unwrap().notification.status,
        NotificationStatus::Sent
    );
    assert_eq!(other.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn queue_limit_rejects_atomically_but_allows_deduplication_and_grouping() {
    let (_dir, mut service, _channel) = setup();
    service.settings.queue_limit = 1;
    let request = Alert {
        group_key: Some("x".into()),
        ..alert()
    };
    let accepted = service
        .alert(request.clone(), Some("same".into()))
        .await
        .unwrap();
    assert_eq!(
        service
            .alert(request.clone(), Some("same".into()))
            .await
            .unwrap()
            .id,
        accepted.id
    );
    assert_eq!(service.alert(request, None).await.unwrap().count, 2);
    assert!(matches!(
        service.alert(alert(), None).await,
        Err(StoreError::QueueFull)
    ));
    let opening = OpenIssue {
        id: "disk".into(),
        ..Default::default()
    };
    assert!(matches!(
        service
            .store
            .open_planned(opening, true, Some(service.plan(Severity::Warning)))
            .await,
        Err(StoreError::QueueFull)
    ));
    assert!(matches!(
        service.store.get("disk".into()).await,
        Err(StoreError::NotFound)
    ));
    due(&service, accepted.id).await;
    service.process(accepted.id).await.unwrap();
    assert_eq!(
        service
            .alert(alert(), None)
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::Sent
    );
}

#[tokio::test]
async fn retention_pins_keyed_deliveries_and_never_deletes_pending_work() {
    let (_dir, mut service, _channel) = setup();
    service.retention = RetentionConfig {
        deliveries: Some(10),
        idempotency_keys: Some(100),
    };
    let keyed = service.alert(alert(), Some("key".into())).await.unwrap();
    let unkeyed = service.alert(alert(), None).await.unwrap();
    let pending = service
        .alert(
            Alert {
                group_key: Some("x".into()),
                ..alert()
            },
            Some("pending".into()),
        )
        .await
        .unwrap();
    service
        .store
        .run(|db| {
            db.execute(
                "UPDATE deliveries SET completed_at=? WHERE state='done'",
                [Utc::now().timestamp() - 20],
            )?;
            db.execute("UPDATE alert_keys SET created_at=0 WHERE key='pending'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.cleanup().await.unwrap();
    assert!(matches!(
        service.get(unkeyed.id).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        service.alert(alert(), Some("key".into())).await.unwrap().id,
        keyed.id
    );
    assert_eq!(
        service.get(pending.id).await.unwrap().notification.status,
        NotificationStatus::Pending
    );
    service
        .store
        .run(|db| {
            db.execute("UPDATE alert_keys SET created_at=0 WHERE key='key'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.cleanup().await.unwrap();
    assert!(matches!(
        service.get(keyed.id).await,
        Err(StoreError::NotFound)
    ));
    assert_ne!(
        service.alert(alert(), Some("key".into())).await.unwrap().id,
        keyed.id
    );
    assert_eq!(
        service
            .store
            .run(|db| Ok(db.query_row(
                "SELECT COUNT(*) FROM alert_keys WHERE key='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .store
            .run(move |db| Ok(db.query_row(
                "SELECT COUNT(*) FROM delivery_targets WHERE delivery_id=?",
                [unkeyed.id],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn scheduler_keeps_partial_progress_when_queue_fills() {
    let (_dir, mut service, _channel) = setup();
    service.settings.queue_limit = 1;
    for id in ["one", "two"] {
        service
            .register_heartbeat(HeartbeatInput {
                id: id.into(),
                title: id.into(),
                interval_seconds: 60,
                grace_seconds: 0,
                severity: Severity::Warning,
                notify_on_recovery: false,
            })
            .await
            .unwrap();
    }
    service
        .store
        .run(|db| {
            db.execute("UPDATE heartbeats SET due=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    service.schedule().await.unwrap();
    let first = jobs(&service).await;
    assert_eq!(first.len(), 1);
    assert_eq!(
        service
            .heartbeats()
            .await
            .unwrap()
            .iter()
            .filter(|b| b.overdue)
            .count(),
        1
    );
    service.process(first[0].id).await.unwrap();
    service.schedule().await.unwrap();
    assert_eq!(jobs(&service).await.len(), 2);
    assert!(
        service
            .heartbeats()
            .await
            .unwrap()
            .iter()
            .all(|b| b.overdue)
    );
}

#[tokio::test]
async fn policy_migration_preserves_legacy_keys_jobs_and_starts_retention_at_upgrade() {
    let (dir, service, channel) = setup();
    let completed = service.alert(alert(), Some("sent".into())).await.unwrap();
    let grouped = Alert {
        group_key: Some("x".into()),
        ..alert()
    };
    let pending = service
        .alert(grouped.clone(), Some("pending".into()))
        .await
        .unwrap();
    drop(service);
    let path = dir.path().join("db.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP INDEX deliveries_completed;
        ALTER TABLE deliveries DROP COLUMN completed_at;
        DROP INDEX alert_keys_created;
        ALTER TABLE alert_keys DROP COLUMN created_at;
        DROP TABLE delivery_targets;
        DROP TABLE destination_rate_limits;",
    )
    .unwrap();
    drop(db);
    let before = Utc::now().timestamp();
    let mut service = DeliveryService::new(Store::open_file(&path).unwrap(), Some(channel.clone()));
    service.retention = RetentionConfig {
        deliveries: Some(60),
        idempotency_keys: Some(60),
    };
    service.cleanup().await.unwrap();
    assert_eq!(
        service
            .alert(alert(), Some("sent".into()))
            .await
            .unwrap()
            .id,
        completed.id
    );
    assert_eq!(
        service
            .alert(grouped, Some("pending".into()))
            .await
            .unwrap()
            .id,
        pending.id
    );
    let created = service
        .store
        .run(|db| {
            Ok(
                db.query_row("SELECT MIN(created_at) FROM alert_keys", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert!(created >= before);
    due(&service, pending.id).await;
    assert_eq!(
        service
            .process(pending.id)
            .await
            .unwrap()
            .notification
            .status,
        NotificationStatus::Sent
    );
    assert_eq!(channel.calls.lock().unwrap().len(), 2);
}
