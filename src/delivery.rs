//! Durable notification delivery, routing, and scheduled checks.
use crate::{
    config::{DeliveryConfig, DestinationConfig, DestinationPolicy, RetentionConfig, ServerConfig},
    models::*,
    notifications::{Notifier, Pushover, Webhook},
    store::{Store, StoreError},
};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS deliveries (
 id INTEGER PRIMARY KEY AUTOINCREMENT, payload TEXT NOT NULL, state TEXT NOT NULL,
 due INTEGER NOT NULL, group_key TEXT, group_until INTEGER, issue_id TEXT, opening_count INTEGER, completed_at INTEGER
);
CREATE INDEX IF NOT EXISTS deliveries_due ON deliveries(state, due);
CREATE TABLE IF NOT EXISTS alert_keys (key TEXT PRIMARY KEY, request TEXT NOT NULL, delivery_id INTEGER NOT NULL REFERENCES deliveries(id), created_at INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS delivery_targets (delivery_id INTEGER NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE, destination TEXT NOT NULL, due INTEGER NOT NULL, PRIMARY KEY(delivery_id,destination));
CREATE TABLE IF NOT EXISTS destination_rate_limits (destination TEXT PRIMARY KEY, bucket INTEGER NOT NULL, window INTEGER NOT NULL, count INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS issue_policies (issue_id TEXT PRIMARY KEY REFERENCES issues(id), payload TEXT NOT NULL, next_reminder INTEGER);
CREATE TABLE IF NOT EXISTS heartbeats (id TEXT PRIMARY KEY, payload TEXT NOT NULL, due INTEGER NOT NULL, overdue INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS delivery_metrics (id INTEGER PRIMARY KEY CHECK(id=1), attempts INTEGER NOT NULL DEFAULT 0, sent INTEGER NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0, skipped INTEGER NOT NULL DEFAULT 0, latency_ms INTEGER NOT NULL DEFAULT 0, minute INTEGER NOT NULL DEFAULT 0, minute_count INTEGER NOT NULL DEFAULT 0);
INSERT OR IGNORE INTO delivery_metrics(id) VALUES(1);
UPDATE deliveries SET state='pending' WHERE state='running';
";

pub(crate) fn migrate_grouping(db: &Connection) -> Result<(), rusqlite::Error> {
    let columns = db
        .prepare("PRAGMA table_info(deliveries)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == "group_until") {
        // Legacy due times may already have been deferred. Keep those deliveries,
        // but do not guess a grouping deadline or admit any more alerts into them.
        db.execute_batch("ALTER TABLE deliveries ADD COLUMN group_until INTEGER;")?;
    }
    db.execute_batch(
        "DROP INDEX IF EXISTS deliveries_group;
        CREATE INDEX IF NOT EXISTS deliveries_group_window
        ON deliveries(group_key, state, group_until);",
    )
}

pub(crate) fn migrate_policies(db: &Connection) -> Result<(), rusqlite::Error> {
    for (table, column, definition) in [
        ("deliveries", "completed_at", "INTEGER"),
        ("alert_keys", "created_at", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let columns = db
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.iter().any(|c| c == column) {
            db.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    let now = Utc::now().timestamp();
    db.execute(
        "UPDATE deliveries SET completed_at=? WHERE state='done' AND completed_at IS NULL",
        [now],
    )?;
    db.execute(
        "UPDATE alert_keys SET created_at=? WHERE created_at=0",
        [now],
    )?;
    db.execute_batch("CREATE INDEX IF NOT EXISTS deliveries_completed ON deliveries(state,completed_at);
        CREATE INDEX IF NOT EXISTS alert_keys_created ON alert_keys(created_at);
        INSERT OR IGNORE INTO delivery_targets(delivery_id,destination,due)
        SELECT d.id,json_extract(t.value,'$.destination'),d.due FROM deliveries d,json_each(d.payload,'$.destinations') t WHERE d.state<>'done';")?;
    Ok(())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| StoreError::Unavailable)
}
fn decode<T: serde::de::DeserializeOwned>(value: String) -> Result<T, StoreError> {
    serde_json::from_str(&value).map_err(|_| StoreError::Unavailable)
}
pub(crate) fn load(db: &Connection, id: i64) -> Result<Delivery, StoreError> {
    let payload = db
        .query_row("SELECT payload FROM deliveries WHERE id=?", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or(StoreError::NotFound)?;
    decode(payload)
}
fn save(db: &Connection, job: &Delivery, state: &str) -> Result<(), StoreError> {
    db.execute(
        "UPDATE deliveries SET payload=?1, state=?2, due=?3, completed_at=CASE WHEN ?2='done' THEN COALESCE(completed_at,?5) ELSE NULL END WHERE id=?4",
        params![encode(job)?, state, job.next_attempt_at, job.id, Utc::now().timestamp()],
    )?;
    db.execute("UPDATE notifications SET status=?1,error=?2 WHERE (issue_id,opening_count) IN (SELECT issue_id,opening_count FROM deliveries WHERE id=?3)", params![job.notification.status.as_str(), job.notification.error,job.id])?;
    Ok(())
}

#[derive(Clone)]
pub struct Plan {
    pub destinations: Vec<String>,
    pub settings: DeliveryConfig,
}
pub(crate) fn enqueue(
    db: &Connection,
    alert: &Alert,
    kind: &str,
    plan: &Plan,
    link: Option<(&str, i64)>,
) -> Result<Delivery, StoreError> {
    let now = Utc::now().timestamp();
    let outcomes: Vec<_> = plan
        .destinations
        .iter()
        .map(|name| DestinationOutcome {
            destination: name.clone(),
            attempts: 0,
            notification: Notification {
                status: NotificationStatus::Pending,
                error: None,
            },
        })
        .collect();
    if kind == "alert"
        && plan.settings.group_window_seconds > 0
        && !outcomes.is_empty()
        && let Some(key) = &alert.group_key
    {
        let ids = db.prepare("SELECT id FROM deliveries WHERE group_key=? AND state='pending' AND group_until>? ORDER BY id DESC")?.query_map(params![key,now], |r| r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in ids {
            let mut job = load(db, id)?;
            if job.severity == alert.severity
                && job.destinations.iter().all(|d| d.attempts == 0)
                && job
                    .destinations
                    .iter()
                    .map(|d| &d.destination)
                    .eq(plan.destinations.iter())
            {
                job.count += 1;
                job.title = alert.title.clone();
                job.message = alert.message.clone();
                save(db, &job, "pending")?;
                return Ok(job);
            }
        }
    }
    if !outcomes.is_empty() && plan.settings.queue_limit > 0 {
        let queued: u64 = db.query_row(
            "SELECT COUNT(*) FROM deliveries WHERE state<>'done'",
            [],
            |r| r.get(0),
        )?;
        if queued >= plan.settings.queue_limit {
            return Err(StoreError::QueueFull);
        }
    }
    let notification = if outcomes.is_empty() {
        Notification::not_attempted()
    } else {
        Notification {
            status: NotificationStatus::Pending,
            error: None,
        }
    };
    let grouped = kind == "alert"
        && alert.group_key.is_some()
        && !outcomes.is_empty()
        && plan.settings.group_window_seconds > 0;
    let group_until = grouped.then_some(now + plan.settings.group_window_seconds as i64);
    let mut job = Delivery {
        id: 0,
        title: alert.title.clone(),
        message: alert.message.clone(),
        severity: alert.severity,
        kind: kind.into(),
        count: 1,
        created_at: now,
        next_attempt_at: group_until.unwrap_or(now),
        notification,
        destinations: outcomes,
    };
    let state = if job.destinations.is_empty() {
        "done"
    } else {
        "pending"
    };
    db.execute("INSERT INTO deliveries(payload,state,due,group_key,group_until,issue_id,opening_count) VALUES('',?,?,?,?,?,?)", params![state,job.next_attempt_at,if kind == "alert" {alert.group_key.as_deref()} else {None},group_until,link.map(|v|v.0),link.map(|v|v.1)])?;
    job.id = db.last_insert_rowid();
    for destination in &job.destinations {
        db.execute(
            "INSERT INTO delivery_targets VALUES(?,?,?)",
            params![job.id, destination.destination, job.next_attempt_at],
        )?;
    }
    save(db, &job, state)?;
    if job.destinations.is_empty() {
        db.execute(
            "UPDATE delivery_metrics SET skipped=skipped+1 WHERE id=1",
            [],
        )?;
    }
    Ok(job)
}

#[derive(Clone)]
struct Channel {
    notifier: Arc<dyn Notifier>,
    capacity: Arc<tokio::sync::Semaphore>,
}

impl Channel {
    fn new(notifier: Arc<dyn Notifier>) -> Self {
        Self {
            capacity: Arc::new(tokio::sync::Semaphore::new(
                notifier.max_concurrency().max(1),
            )),
            notifier,
        }
    }
}

#[derive(Clone)]
pub struct DeliveryService {
    pub store: Store,
    pub settings: DeliveryConfig,
    channels: BTreeMap<String, Channel>,
    permits: Arc<tokio::sync::Semaphore>,
    defaults: Vec<String>,
    routes: BTreeMap<Severity, Vec<String>>,
    policies: BTreeMap<String, DestinationPolicy>,
    retention: RetentionConfig,
}
impl DeliveryService {
    pub fn new(store: Store, notifier: Option<Arc<dyn Notifier>>) -> Self {
        let mut channels = BTreeMap::new();
        if let Some(notifier) = notifier {
            channels.insert("pushover".into(), Channel::new(notifier));
        }
        Self {
            store,
            settings: DeliveryConfig::default(),
            defaults: channels.keys().cloned().collect(),
            channels,
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
            routes: BTreeMap::new(),
            policies: BTreeMap::new(),
            retention: RetentionConfig::default(),
        }
    }
    pub fn configured(store: Store, config: &ServerConfig) -> anyhow::Result<Self> {
        let mut channels = BTreeMap::new();
        for (name, destination) in &config.destinations {
            let channel: Arc<dyn Notifier> = match destination {
                DestinationConfig::Pushover { config } => Arc::new(Pushover::new(config.clone())?),
                DestinationConfig::Webhook { url, bearer_token } => {
                    Arc::new(Webhook::new(url.clone(), bearer_token.clone())?)
                }
            };
            channels.insert(name.clone(), Channel::new(channel));
        }
        Ok(Self {
            store,
            settings: config.delivery.clone(),
            channels,
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
            defaults: config.default_destinations.clone(),
            routes: config.routes.clone(),
            policies: config.destination_policies.clone(),
            retention: config.retention.clone(),
        })
    }
    pub fn plan(&self, severity: Severity) -> Plan {
        Plan {
            destinations: self.routes.get(&severity).unwrap_or(&self.defaults).clone(),
            settings: self.settings.clone(),
        }
    }
    pub async fn alert(&self, request: Alert, key: Option<String>) -> Result<Delivery, StoreError> {
        let plan = self.plan(request.severity);
        let job = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                let fingerprint = encode(&request)?;
                if let Some(key) = &key
                    && let Some((previous, id)) = tx
                        .query_row(
                            "SELECT request,delivery_id FROM alert_keys WHERE key=?",
                            [key],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                        )
                        .optional()?
                {
                    if previous != fingerprint {
                        return Err(StoreError::Conflict);
                    }
                    return load(&tx, id);
                }
                let job = enqueue(&tx, &request, "alert", &plan, None)?;
                if let Some(key) = key {
                    tx.execute(
                        "INSERT INTO alert_keys(key,request,delivery_id,created_at) VALUES(?,?,?,?)",
                        params![key, fingerprint, job.id, Utc::now().timestamp()],
                    )?;
                }
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.process(job.id).await
    }
    pub async fn get(&self, id: i64) -> Result<Delivery, StoreError> {
        self.store.run(move |db| load(db, id)).await
    }

    pub async fn process(&self, id: i64) -> Result<Delivery, StoreError> {
        let Ok(_permit) = self.permits.try_acquire() else {
            return self.get(id).await;
        };
        let claimed = self.store.run(move |db| Ok(db.execute("UPDATE deliveries SET state='running' WHERE id=? AND state='pending' AND due<=?",params![id,Utc::now().timestamp()])? == 1)).await?;
        if !claimed {
            return self.get(id).await;
        }
        let result = self.process_claimed(id).await;
        if result.is_err() {
            // A storage failure must not strand the job until restart.
            let _ = self
                .store
                .run(move |db| {
                    db.execute(
                        "UPDATE deliveries SET state='pending' WHERE id=? AND state='running'",
                        [id],
                    )?;
                    Ok(())
                })
                .await;
        }
        result
    }
    fn policy(&self, name: &str) -> DestinationPolicy {
        self.policies
            .get(name)
            .cloned()
            .unwrap_or_else(|| DestinationPolicy::from(&self.settings))
    }
    async fn process_claimed(&self, id: i64) -> Result<Delivery, StoreError> {
        let mut job = self.get(id).await?;
        let mut due: BTreeMap<String, i64> = self
            .store
            .run(move |db| {
                Ok(db
                    .prepare("SELECT destination,due FROM delivery_targets WHERE delivery_id=?")?
                    .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                    .collect::<Result<_, _>>()?)
            })
            .await?;
        for index in 0..job.destinations.len() {
            let name = job.destinations[index].destination.clone();
            let policy = self.policy(&name);
            if matches!(
                job.destinations[index].notification.status,
                NotificationStatus::Sent | NotificationStatus::NotAttempted
            ) || job.destinations[index].attempts >= policy.max_attempts
            {
                continue;
            }
            if due.get(&name).copied().unwrap_or(0) > Utc::now().timestamp() {
                continue;
            }
            let channel = self.channels.get(&name).cloned();
            let capacity = match &channel {
                Some(channel) => Some(
                    channel
                        .capacity
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|_| StoreError::Unavailable)?,
                ),
                None => None,
            };
            let global_limit = self.settings.rate_limit_per_minute;
            let window = self.settings.rate_window_seconds;
            let destination_limit = policy.rate_limit.clone();
            let destination = name.clone();
            let deferred=self.store.run(move |db| {
                let tx=db.transaction()?;
                let now=Utc::now().timestamp();
                let bucket=now/window as i64;
                tx.execute("UPDATE delivery_metrics SET minute=?1,minute_count=0 WHERE id=1 AND minute<>?1",[bucket])?;
                let count:u32=tx.query_row("SELECT minute_count FROM delivery_metrics WHERE id=1",[],|r|r.get(0))?;
                let mut blocked_until=if global_limit>0 && count>=global_limit {Some((bucket+1)*window as i64)} else {None};
                if let Some(rate)=&destination_limit {
                    let bucket=now/rate.window_seconds as i64;
                    tx.execute("INSERT INTO destination_rate_limits(destination,bucket,window,count) VALUES(?,?,?,0) ON CONFLICT(destination) DO UPDATE SET bucket=excluded.bucket,window=excluded.window,count=0 WHERE bucket<>excluded.bucket OR window<>excluded.window",params![destination,bucket,rate.window_seconds])?;
                    let count:u32=tx.query_row("SELECT count FROM destination_rate_limits WHERE destination=?",[&destination],|r|r.get(0))?;
                    if rate.attempts>0 && count>=rate.attempts {blocked_until=Some(blocked_until.unwrap_or(0).max((bucket+1)*rate.window_seconds as i64));}
                }
                if let Some(until)=blocked_until {
                    tx.execute("UPDATE delivery_targets SET due=? WHERE delivery_id=? AND destination=?",params![until,id,destination])?;
                    tx.commit()?;
                    return Ok(Some(until));
                }
                tx.execute("UPDATE delivery_metrics SET minute_count=minute_count+1,attempts=attempts+1 WHERE id=1",[])?;
                if destination_limit.is_some() {tx.execute("UPDATE destination_rate_limits SET count=count+1 WHERE destination=?",[&destination])?;}
                let mut job=load(&tx,id)?;
                job.destinations[index].attempts+=1;
                save(&tx,&job,"running")?;
                tx.commit()?;
                Ok(None)
            }).await?;
            if let Some(until) = deferred {
                due.insert(name, until);
                continue;
            }
            job.destinations[index].attempts += 1;
            let started = Instant::now();
            let mut event = job.clone();
            if event.count > 1 {
                event.message =
                    format!("{} alerts grouped. Latest: {}", event.count, event.message)
                        .chars()
                        .take(1024)
                        .collect();
            }
            let outcome = if let Some(channel) = channel {
                tokio::spawn(async move {
                    let _capacity = capacity;
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        channel.notifier.send_delivery(&event),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Notification::failed("Notification timed out; delivery is uncertain.")
                    })
                })
                .await
                .unwrap_or_else(|_| {
                    Notification::failed("Notification interrupted; delivery is uncertain.")
                })
            } else {
                Notification::failed("Configured destination is unavailable.")
            };
            let outcome = if matches!(
                outcome.status,
                NotificationStatus::Sent | NotificationStatus::Failed
            ) {
                outcome
            } else {
                Notification::failed("Invalid notification channel outcome.")
            };
            let elapsed = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
            let attempts = job.destinations[index].attempts;
            tracing::info!(delivery_id=id,destination=%name,status=outcome.status.as_str(),attempt=attempts,latency_ms=elapsed,"Notification attempt completed");
            let next = Utc::now().timestamp()
                + policy
                    .retry_base_seconds
                    .saturating_mul(1u64 << attempts.saturating_sub(1).min(20))
                    .min(policy.retry_max_seconds) as i64;
            due.insert(name.clone(), next);
            job.destinations[index].notification = outcome.clone();
            let saved = job.clone();
            self.store.run(move |db| {
                let tx=db.transaction()?;
                tx.execute("UPDATE delivery_metrics SET sent=sent+?1,failed=failed+?2,latency_ms=latency_ms+?3 WHERE id=1",params![i64::from(outcome.status==NotificationStatus::Sent),i64::from(outcome.status==NotificationStatus::Failed),elapsed])?;
                tx.execute("UPDATE delivery_targets SET due=? WHERE delivery_id=? AND destination=?",params![next,id,name])?;
                save(&tx,&saved,"running")?;
                tx.commit()?;
                Ok(())
            }).await?;
        }
        let mut next = None;
        for destination in &mut job.destinations {
            let policy = self.policy(&destination.destination);
            if destination.attempts >= policy.max_attempts
                && destination.notification.status == NotificationStatus::Pending
            {
                destination.notification = Notification::failed(
                    "Attempt interrupted; delivery is uncertain and the attempt limit was reached.",
                );
            }
            if !matches!(
                destination.notification.status,
                NotificationStatus::Sent | NotificationStatus::NotAttempted
            ) && destination.attempts < policy.max_attempts
            {
                let target_due = due
                    .get(&destination.destination)
                    .copied()
                    .unwrap_or_else(|| Utc::now().timestamp());
                next = Some(next.map_or(target_due, |previous: i64| previous.min(target_due)));
            }
        }
        job.notification = if next.is_some() {
            Notification {
                status: NotificationStatus::Pending,
                error: None,
            }
        } else if job
            .destinations
            .iter()
            .all(|d| d.notification.status == NotificationStatus::Sent)
        {
            Notification::sent()
        } else {
            job.destinations
                .iter()
                .find(|d| d.notification.status == NotificationStatus::Failed)
                .map(|d| d.notification.clone())
                .unwrap_or_else(Notification::not_attempted)
        };
        if let Some(next) = next {
            job.next_attempt_at = next;
        }
        let saved = job.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                save(&tx, &saved, if next.is_some() { "pending" } else { "done" })?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(job)
    }

    pub async fn cleanup(&self) -> Result<(), StoreError> {
        let retention = self.retention.clone();
        self.store.run(move |db| {
            let tx=db.transaction()?;
            let now=Utc::now().timestamp();
            // Keys pin their delivery records. Neither age policy can delete active work.
            if let Some(age)=retention.idempotency_keys {
                tx.execute("DELETE FROM alert_keys WHERE created_at<=? AND delivery_id IN (SELECT id FROM deliveries WHERE state='done')",[now-age as i64])?;
            }
            if let Some(age)=retention.deliveries {
                tx.execute("DELETE FROM deliveries WHERE state='done' AND completed_at<=? AND NOT EXISTS (SELECT 1 FROM alert_keys k WHERE k.delivery_id=deliveries.id)",[now-age as i64])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn metrics(&self) -> Result<String, StoreError> {
        self.store.run(|db| {
            let (attempts,sent,failed,skipped,latency):(i64,i64,i64,i64,i64) = db.query_row("SELECT attempts,sent,failed,skipped,latency_ms FROM delivery_metrics WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            let queued:i64 = db.query_row("SELECT COUNT(*) FROM deliveries WHERE state<>'done'",[],|r|r.get(0))?;
            Ok(format!("# TYPE flares_notification_attempts_total counter\nflares_notification_attempts_total {attempts}\n# TYPE flares_notifications_total counter\nflares_notifications_total{{status=\"sent\"}} {sent}\nflares_notifications_total{{status=\"failed\"}} {failed}\nflares_notifications_total{{status=\"skipped\"}} {skipped}\n# TYPE flares_notification_latency_seconds summary\nflares_notification_latency_seconds_sum {}\nflares_notification_latency_seconds_count {}\n# TYPE flares_delivery_queue_depth gauge\nflares_delivery_queue_depth {queued}\n",latency as f64/1000.0,sent+failed))
        }).await
    }
    pub async fn run(self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut active = tokio::task::JoinSet::new();
        let mut maintenance = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _=maintenance.tick()=> {if let Err(error)=self.cleanup().await {tracing::error!(error=%error,"Delivery cleanup failed");}}
                result=stop.changed()=> { if result.is_err() || *stop.borrow() {break;} }
                Some(result)=active.join_next(), if !active.is_empty()=> {if result.is_err() {tracing::error!("Delivery task interrupted");}}
                _=interval.tick()=> {
                    if let Err(error)=self.schedule().await {tracing::error!(error=%error,"Scheduled notification check failed");}
                    let capacity=4usize.saturating_sub(active.len());
                    if capacity == 0 {continue;}
                    let ids=self.store.run(move |db| Ok(db.prepare("SELECT id FROM deliveries WHERE state='pending' AND due<=? ORDER BY due,id LIMIT ?")?.query_map(params![Utc::now().timestamp(),capacity as i64],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?)).await;
                    match ids {Ok(ids)=>for id in ids {let service=self.clone(); active.spawn(async move {if let Err(error)=service.process(id).await {tracing::error!(delivery_id=id,error=%error,"Delivery processing failed");}});},Err(error)=>tracing::error!(error=%error,"Delivery queue read failed")}
                }
            }
        }
        while active.join_next().await.is_some() {}
    }
    pub async fn schedule(&self) -> Result<(), StoreError> {
        let service = self.clone();
        self.store.run(move |db| {
            let tx=db.transaction()?;
            let now=Utc::now().timestamp();
            let policies=tx.prepare("SELECT p.payload,i.title,i.message FROM issue_policies p JOIN issues i ON i.id=p.issue_id WHERE i.status='open' AND p.next_reminder<=?")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
            for (payload,title,message) in policies {
                let policy:OpenIssue=decode(payload)?;
                match enqueue(&tx,&Alert {title,message:format!("Reminder: {message}").chars().take(1024).collect(),severity:policy.severity,group_key:None},"reminder",&service.plan(policy.severity),None) {Ok(_)=>{},Err(StoreError::QueueFull)=>break,Err(error)=>return Err(error)};
                tx.execute("UPDATE issue_policies SET next_reminder=? WHERE issue_id=?",params![now+policy.remind_every_seconds.unwrap_or(3600) as i64,policy.id])?;
            }
            let beats=tx.prepare("SELECT payload FROM heartbeats WHERE overdue=0 AND due<=?")?.query_map([now],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            for payload in beats {
                let mut beat:Heartbeat=decode(payload)?;
                beat.overdue=true;
                match enqueue(&tx,&Alert {title:beat.config.title.clone(),message:format!("Heartbeat {} missed its deadline.",beat.config.id),severity:beat.config.severity,group_key:None},"heartbeat_missed",&service.plan(beat.config.severity),None) {Ok(_)=>{},Err(StoreError::QueueFull)=>break,Err(error)=>return Err(error)};
                tx.execute("UPDATE heartbeats SET payload=?,overdue=1 WHERE id=?",params![encode(&beat)?,beat.config.id])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
    pub async fn register_heartbeat(
        &self,
        config: HeartbeatInput,
    ) -> Result<Heartbeat, StoreError> {
        self.store.run(move |db| {
            let now=Utc::now().timestamp();
            let beat=Heartbeat {last_seen:now,due_at:now+(config.interval_seconds+config.grace_seconds) as i64,overdue:false,config};
            db.execute("INSERT INTO heartbeats(id,payload,due,overdue) VALUES(?,?,?,0) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload,due=excluded.due,overdue=0",params![beat.config.id,encode(&beat)?,beat.due_at])?;
            Ok(beat)
        }).await
    }
    pub async fn check_in(&self, id: String) -> Result<Heartbeat, StoreError> {
        let service = self.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let payload = tx
                    .query_row("SELECT payload FROM heartbeats WHERE id=?", [&id], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .ok_or(StoreError::NotFound)?;
                let mut beat: Heartbeat = decode(payload)?;
                if beat.overdue && beat.config.notify_on_recovery {
                    enqueue(
                        &tx,
                        &Alert {
                            title: beat.config.title.clone(),
                            message: format!("Heartbeat {} recovered.", beat.config.id),
                            severity: beat.config.severity,
                            group_key: None,
                        },
                        "heartbeat_recovered",
                        &service.plan(beat.config.severity),
                        None,
                    )?;
                }
                beat.last_seen = Utc::now().timestamp();
                beat.due_at = beat.last_seen
                    + (beat.config.interval_seconds + beat.config.grace_seconds) as i64;
                beat.overdue = false;
                tx.execute(
                    "UPDATE heartbeats SET payload=?,due=?,overdue=0 WHERE id=?",
                    params![encode(&beat)?, beat.due_at, id],
                )?;
                tx.commit()?;
                Ok(beat)
            })
            .await
    }
    pub async fn heartbeats(&self) -> Result<Vec<Heartbeat>, StoreError> {
        self.store
            .run(|db| {
                db.prepare("SELECT payload FROM heartbeats ORDER BY id")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .map(|r| decode(r?))
                    .collect()
            })
            .await
    }
    pub async fn delete_heartbeat(&self, id: String) -> Result<(), StoreError> {
        self.store
            .run(move |db| {
                if db.execute("DELETE FROM heartbeats WHERE id=?", [id])? == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
