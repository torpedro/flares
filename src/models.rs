use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Open,
    Closed,
}

impl IssueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    Pending,
    Sent,
    Failed,
    Unknown,
    NotAttempted,
}

impl NotificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::NotAttempted => "not_attempted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct Notification {
    pub status: NotificationStatus,
    pub error: Option<String>,
}

impl Notification {
    pub fn sent() -> Self {
        Self {
            status: NotificationStatus::Sent,
            error: None,
        }
    }

    pub fn failed(error: &str) -> Self {
        Self {
            status: NotificationStatus::Failed,
            error: Some(error.to_owned()),
        }
    }

    pub fn not_attempted() -> Self {
        Self {
            status: NotificationStatus::NotAttempted,
            error: None,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenIssue {
    #[schema(min_length = 1, max_length = 200)]
    pub id: String,
    #[schema(min_length = 1, max_length = 250)]
    pub title: Option<String>,
    #[schema(min_length = 1, max_length = 1024)]
    pub message: Option<String>,
    #[serde(default)]
    pub severity: Severity,
    pub remind_every_seconds: Option<u64>,
    #[serde(default)]
    pub notify_on_resolution: bool,
}

pub fn validate_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() || id.chars().count() > 200 {
        Err("id must contain between 1 and 200 characters")
    } else {
        Ok(())
    }
}

impl OpenIssue {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_id(&self.id)?;
        validate_interval(self.remind_every_seconds)?;
        if self
            .title
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.chars().count() > 250)
        {
            return Err("title must contain between 1 and 250 characters");
        }
        if self
            .message
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.chars().count() > 1024)
        {
            return Err("message must contain between 1 and 1024 characters");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseIssue {
    #[schema(min_length = 1, max_length = 200)]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Issue {
    pub severity: Severity,
    pub remind_every_seconds: Option<u64>,
    pub notify_on_resolution: bool,
    pub delivery_id: Option<i64>,
    pub id: String,
    pub status: IssueStatus,
    pub title: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub opening_count: i64,
    pub notification: Notification,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MutationResult {
    pub delivery_id: Option<i64>,
    pub issue: Issue,
    pub changed: bool,
    /// The attempt made by this request, not the latest historical outcome.
    pub notification: Notification,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Alert {
    #[serde(default)]
    pub severity: Severity,
    pub group_key: Option<String>,
    #[schema(min_length = 1, max_length = 250)]
    pub title: String,
    #[schema(min_length = 1, max_length = 1024)]
    pub message: String,
}

impl Alert {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(key) = &self.group_key {
            validate_id(key)?;
        }
        if self.title.is_empty() || self.title.chars().count() > 250 {
            return Err("title must contain between 1 and 250 characters");
        }
        if self.message.is_empty() || self.message.chars().count() > 1024 {
            return Err("message must contain between 1 and 1024 characters");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AlertResult {
    pub delivery_id: i64,
    pub notification: Notification,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IssueList {
    pub items: Vec<Issue>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub status: Option<IssueStatus>,
    #[serde(default = "default_limit")]
    #[param(minimum = 1, maximum = 1000, default = 100)]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    100
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub detail: String,
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ToSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    #[default]
    Warning,
    Critical,
}

pub fn validate_interval(value: Option<u64>) -> Result<(), &'static str> {
    if value.is_some_and(|value| !(1..=31_536_000).contains(&value)) {
        return Err("interval must be between 1 and 31536000 seconds");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatInput {
    pub id: String,
    pub title: String,
    pub interval_seconds: u64,
    #[serde(default)]
    pub grace_seconds: u64,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default)]
    pub notify_on_recovery: bool,
}
impl HeartbeatInput {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_id(&self.id)?;
        if self.title.is_empty() || self.title.chars().count() > 250 {
            return Err("title must contain between 1 and 250 characters");
        }
        validate_interval(Some(self.interval_seconds))?;
        if self.grace_seconds > 31_536_000 {
            return Err("grace_seconds must be at most 31536000");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Heartbeat {
    #[serde(flatten)]
    pub config: HeartbeatInput,
    pub last_seen: i64,
    pub due_at: i64,
    pub overdue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DestinationOutcome {
    pub destination: String,
    pub attempts: u32,
    pub notification: Notification,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Delivery {
    pub id: i64,
    pub title: String,
    pub message: String,
    pub severity: Severity,
    pub kind: String,
    pub count: u64,
    pub created_at: i64,
    pub next_attempt_at: i64,
    pub notification: Notification,
    pub destinations: Vec<DestinationOutcome>,
}
