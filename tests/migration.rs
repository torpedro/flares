use flares::{
    models::{Notification, NotificationStatus, OpenIssue},
    store::Store,
};

#[tokio::test]
async fn legacy_database_preserves_history_and_allows_disabled_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    // Original schema, before not_attempted was a persisted status.
    db.execute_batch(
        "CREATE TABLE issues (
        id TEXT PRIMARY KEY, status TEXT NOT NULL, title TEXT NOT NULL, message TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, opened_at TEXT NOT NULL,
        closed_at TEXT, opening_count INTEGER NOT NULL
    );
    CREATE TABLE notifications (
        issue_id TEXT NOT NULL REFERENCES issues(id), opening_count INTEGER NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('pending', 'sent', 'failed', 'unknown')),
        error TEXT, PRIMARY KEY(issue_id, opening_count)
    );
    INSERT INTO issues VALUES ('existing', 'closed', 'Title', 'Message',
        '2026-09-14T12:00:00Z', '2026-09-14T12:00:00Z', '2026-09-14T12:00:00Z',
        '2026-09-14T12:00:00Z', 2);
    INSERT INTO notifications VALUES ('existing', 1, 'sent', NULL);
    INSERT INTO notifications VALUES ('existing', 2, 'failed', 'Original error');",
    )
    .unwrap();
    drop(db);
    let store = Store::open_file(&path).unwrap();
    let old = store.get("existing".into()).await.unwrap();
    assert_eq!(old.notification.status, NotificationStatus::Failed);
    assert_eq!(old.notification.error.as_deref(), Some("Original error"));
    let (reopened, changed) = store
        .open(
            OpenIssue {
                id: "existing".into(),
                title: None,
                message: None,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert!(changed);
    assert_eq!(reopened.opening_count, 3);
    assert_eq!(reopened.created_at, old.created_at);
    assert_eq!(reopened.notification, Notification::not_attempted());
    drop(store);
    let store = Store::open_file(&path).unwrap();
    assert_eq!(
        store.get("existing".into()).await.unwrap().notification,
        Notification::not_attempted()
    );
    store.close("existing".into()).await.unwrap();
    let (enabled, _) = store
        .open(
            OpenIssue {
                id: "existing".into(),
                title: None,
                message: None,
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(enabled.notification.status, NotificationStatus::Pending);
    let db = rusqlite::Connection::open(&path).unwrap();
    let statuses: Vec<String> = db
        .prepare("SELECT status FROM notifications ORDER BY opening_count")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(statuses, ["sent", "failed", "not_attempted", "pending"]);
}
