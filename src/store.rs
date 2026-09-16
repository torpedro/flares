use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::models::*;
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Issue not found")]
    NotFound,
    #[error("Idempotency key was already used with a different request")]
    Conflict,
    #[error("Issue storage is unavailable")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Issue storage is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

const SELECT: &str = "SELECT i.id, i.status, i.title, i.message, i.created_at, i.updated_at,
    i.opened_at, i.closed_at, i.opening_count, n.status, n.error,
    (SELECT payload FROM issue_policies WHERE issue_id=i.id),
    (SELECT id FROM deliveries WHERE issue_id=i.id AND opening_count=i.opening_count LIMIT 1)
    FROM issues i JOIN notifications n ON n.issue_id = i.id AND n.opening_count = i.opening_count";

const NOTIFICATIONS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS notifications (
    issue_id TEXT NOT NULL REFERENCES issues(id), opening_count INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'sent', 'failed', 'unknown', 'not_attempted')),
    error TEXT, PRIMARY KEY(issue_id, opening_count)
);";

fn parse<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(text)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn issue(row: &Row<'_>) -> rusqlite::Result<Issue> {
    let policy: OpenIssue = row
        .get::<_, Option<String>>(11)?
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .unwrap_or_default();
    Ok(Issue {
        severity: policy.severity,
        remind_every_seconds: policy.remind_every_seconds,
        notify_on_resolution: policy.notify_on_resolution,
        delivery_id: row.get(12)?,
        id: row.get(0)?,
        status: parse(row, 1)?,
        title: row.get(2)?,
        message: row.get(3)?,
        created_at: parse(row, 4)?,
        updated_at: parse(row, 5)?,
        opened_at: parse(row, 6)?,
        closed_at: if row.get::<_, Option<String>>(7)?.is_some() {
            Some(parse(row, 7)?)
        } else {
            None
        },
        opening_count: row.get(8)?,
        notification: Notification {
            status: parse(row, 9)?,
            error: row.get(10)?,
        },
    })
}

fn get(db: &Connection, id: &str) -> Result<Issue, StoreError> {
    db.query_row(&format!("{SELECT} WHERE i.id = ?"), [id], issue)
        .optional()?
        .ok_or(StoreError::NotFound)
}

impl Store {
    /// Initialize once per service instance and recover unfinished durable deliveries.
    pub fn open_file(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|_| anyhow::anyhow!("Cannot create database directory"))?;
        }
        let mut db =
            Connection::open(path).map_err(|_| anyhow::anyhow!("Cannot open SQLite database"))?;
        db.busy_timeout(Duration::from_secs(10))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS issues (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL CHECK(status IN ('open', 'closed')),
                title TEXT NOT NULL, message TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, opened_at TEXT NOT NULL,
                closed_at TEXT, opening_count INTEGER NOT NULL CHECK(opening_count > 0)
            );
            CREATE INDEX IF NOT EXISTS issues_updated ON issues(updated_at DESC, id);
            CREATE INDEX IF NOT EXISTS issues_status_updated ON issues(status, updated_at DESC, id);",
        )
        .map_err(|_| anyhow::anyhow!("Cannot initialize SQLite database"))?;
        // SQLite cannot alter a CHECK constraint in place. Preserve all outcomes when
        // upgrading databases created before disabled notifications were supported.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let schema: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='notifications'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if schema.is_some_and(|sql| !sql.contains("'not_attempted'")) {
            tx.execute_batch("ALTER TABLE notifications RENAME TO notifications_legacy;")?;
            tx.execute_batch(NOTIFICATIONS_SCHEMA)?;
            tx.execute_batch(
                "INSERT INTO notifications SELECT * FROM notifications_legacy;
                DROP TABLE notifications_legacy;",
            )?;
        } else {
            tx.execute_batch(NOTIFICATIONS_SCHEMA)?;
        }
        tx.execute_batch(crate::delivery::SCHEMA)?;
        crate::delivery::migrate_grouping(&tx)?;
        tx.execute_batch(
            "UPDATE notifications SET status = 'unknown',
            error = 'Service stopped before the notification outcome was recorded; not retried.'
            WHERE status = 'pending' AND NOT EXISTS (SELECT 1 FROM deliveries d WHERE d.issue_id=notifications.issue_id AND d.opening_count=notifications.opening_count);",
        )?;
        tx.commit()?;
        Ok(Self(Arc::new(Mutex::new(db))))
    }

    pub(crate) async fn run<T, F>(&self, action: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = store.0.lock().map_err(|_| StoreError::Unavailable)?;
            action(&mut db)
        })
        .await
        .map_err(|_| StoreError::Unavailable)?
    }

    pub async fn open(
        &self,
        request: OpenIssue,
        notify: bool,
    ) -> Result<(Issue, bool), StoreError> {
        let (issue, changed, _) = self.open_planned(request, notify, None).await?;
        Ok((issue, changed))
    }

    pub async fn open_planned(
        &self,
        request: OpenIssue,
        notify: bool,
        plan: Option<crate::delivery::Plan>,
    ) -> Result<(Issue, bool, Option<i64>), StoreError> {
        self.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let policy = serde_json::to_string(&request).map_err(|_| StoreError::Unavailable)?;
            let previous = match get(&tx, &request.id) {
                Ok(value) => Some(value),
                Err(StoreError::NotFound) => None,
                Err(e) => return Err(e),
            };
            if let Some(value) = previous.as_ref().filter(|i| i.status == IssueStatus::Open) {
                return Ok((value.clone(), false, None));
            }
            let count = previous.as_ref().map_or(1, |i| i.opening_count + 1);
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
            let title = request.title.unwrap_or_else(|| request.id.clone());
            let action = if previous.is_some() {
                "reopened"
            } else {
                "opened"
            };
            let message = request
                .message
                .unwrap_or_else(|| format!("Issue {} {action}.", request.id));
            tx.execute(
                "INSERT INTO issues
                (id, status, title, message, created_at, updated_at, opened_at, opening_count)
                VALUES (?1, 'open', ?2, ?3, ?4, ?4, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET status='open', title=?2, message=?3,
                    updated_at=?4, opened_at=?4, opening_count=?5",
                params![request.id, title, message, now, count],
            )?;
            let status = if notify { "pending" } else { "not_attempted" };
            tx.execute(
                "INSERT INTO notifications(issue_id, opening_count, status) VALUES (?, ?, ?)",
                params![request.id, count, status],
            )?;
            tx.execute("INSERT INTO issue_policies(issue_id,payload,next_reminder) VALUES(?,?,?) ON CONFLICT(issue_id) DO UPDATE SET payload=excluded.payload,next_reminder=excluded.next_reminder",params![request.id,policy,request.remind_every_seconds.map(|seconds| Utc::now().timestamp()+seconds as i64)])?;
            let delivery_id = if let Some(plan) = plan {
                Some(crate::delivery::enqueue(&tx,&Alert {title,message,severity:request.severity,group_key:None},"issue_opened",&plan,Some((&request.id,count)))?.id)
            } else {None};
            let value = get(&tx, &request.id)?;
            tx.commit()?;
            Ok((value, true, delivery_id))
        })
        .await
    }

    pub async fn close(&self, id: String) -> Result<(Issue, bool), StoreError> {
        let (issue, changed, _) = self.close_planned(id, None).await?;
        Ok((issue, changed))
    }

    pub async fn close_planned(
        &self,
        id: String,
        service: Option<crate::delivery::DeliveryService>,
    ) -> Result<(Issue, bool, Option<i64>), StoreError> {
        self.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous = get(&tx, &id)?;
            if previous.status == IssueStatus::Closed {
                return Ok((previous, false, None));
            }
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
            tx.execute(
                "UPDATE issues SET status='closed', updated_at=?1, closed_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            let mut delivery_id = None;
            let policy: Option<String> = tx
                .query_row(
                    "SELECT payload FROM issue_policies WHERE issue_id=?",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            if let (Some(service), Some(policy)) = (service, policy) {
                let policy: OpenIssue =
                    serde_json::from_str(&policy).map_err(|_| StoreError::Unavailable)?;
                if policy.notify_on_resolution {
                    delivery_id = Some(
                        crate::delivery::enqueue(
                            &tx,
                            &Alert {
                                title: previous.title,
                                message: format!("Issue {id} resolved."),
                                severity: policy.severity,
                                group_key: None,
                            },
                            "issue_resolved",
                            &service.plan(policy.severity),
                            None,
                        )?
                        .id,
                    );
                }
            }
            tx.execute(
                "UPDATE issue_policies SET next_reminder=NULL WHERE issue_id=?",
                [&id],
            )?;
            let value = get(&tx, &id)?;
            tx.commit()?;
            Ok((value, true, delivery_id))
        })
        .await
    }

    pub async fn record_notification(
        &self,
        id: String,
        count: i64,
        result: Notification,
    ) -> Result<(), StoreError> {
        self.run(move |db| {
            db.execute(
                "UPDATE notifications SET status=?, error=? WHERE issue_id=? AND opening_count=?",
                params![result.status.as_str(), result.error, id, count],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn get(&self, id: String) -> Result<Issue, StoreError> {
        self.run(move |db| get(db, &id)).await
    }

    pub async fn list(&self, query: ListQuery) -> Result<IssueList, StoreError> {
        self.run(move |db| {
            let tx = db.transaction()?;
            let status = query.status.map(IssueStatus::as_str);
            let total = tx.query_row(
                "SELECT COUNT(*) FROM issues WHERE (?1 IS NULL OR status=?1)",
                [status],
                |r| r.get(0),
            )?;
            let items = tx
                .prepare(&format!(
                    "{SELECT} WHERE (?1 IS NULL OR i.status=?1)
                ORDER BY i.updated_at DESC, i.id ASC LIMIT ?2 OFFSET ?3"
                ))?
                .query_map(params![status, query.limit, query.offset], issue)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(IssueList {
                items,
                total,
                limit: query.limit,
                offset: query.offset,
            })
        })
        .await
    }
}
