use crate::connection::SessionBinding;
use crate::storage::work::{now, WorkDatabase, WorkDatabaseError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::launchd::{LaunchdError, LaunchdManager};
use crate::storage::work::WorkScheduledTask;

const MIN_INTERVAL_SECONDS: u64 = 60;
const MAX_PROMPT_BYTES: usize = 128 * 1024;
const MAX_TITLE_BYTES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduledTaskSchedule {
    Interval { seconds: u64 },
    Daily { hour: u8, minute: u8 },
    Weekly { weekday: u8, hour: u8, minute: u8 },
}

impl ScheduledTaskSchedule {
    pub fn validate(&self) -> Result<(), ScheduledTaskError> {
        match self {
            Self::Interval { seconds } if *seconds < MIN_INTERVAL_SECONDS => Err(
                ScheduledTaskError::InvalidSchedule("interval must be at least 60 seconds".into()),
            ),
            Self::Interval { .. } => Ok(()),
            Self::Daily { hour, minute } => validate_time(*hour, *minute),
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => {
                if *weekday > 6 {
                    return Err(ScheduledTaskError::InvalidSchedule(
                        "weekday must be between 0 (Sunday) and 6 (Saturday)".into(),
                    ));
                }
                validate_time(*hour, *minute)
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Interval { seconds } => format!("every {seconds} seconds"),
            Self::Daily { hour, minute } => format!("daily at {hour:02}:{minute:02}"),
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => format!("weekly on day {weekday} at {hour:02}:{minute:02}"),
        }
    }
}

fn validate_time(hour: u8, minute: u8) -> Result<(), ScheduledTaskError> {
    if hour > 23 {
        return Err(ScheduledTaskError::InvalidSchedule(
            "hour must be between 0 and 23".into(),
        ));
    }
    if minute > 59 {
        return Err(ScheduledTaskError::InvalidSchedule(
            "minute must be between 0 and 59".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub workspace_root: PathBuf,
    pub project_id: Option<String>,
    pub binding: SessionBinding,
    pub schedule: ScheduledTaskSchedule,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_run_at: Option<i64>,
    pub last_run_success: Option<bool>,
    pub last_error: Option<String>,
    pub last_conversation_id: Option<String>,
}

impl ScheduledTask {
    pub fn new(
        title: impl Into<String>,
        prompt: impl Into<String>,
        workspace_root: PathBuf,
        project_id: Option<String>,
        binding: SessionBinding,
        schedule: ScheduledTaskSchedule,
    ) -> Self {
        let timestamp = now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            prompt: prompt.into(),
            workspace_root,
            project_id,
            binding,
            schedule,
            enabled: true,
            created_at: timestamp,
            updated_at: timestamp,
            last_run_at: None,
            last_run_success: None,
            last_error: None,
            last_conversation_id: None,
        }
    }

    pub fn validate(&self) -> Result<(), ScheduledTaskError> {
        if self.id.trim().is_empty()
            || self.id.len() > 100
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ScheduledTaskError::InvalidTask(
                "id must contain only letters, numbers, '-' or '_'".into(),
            ));
        }
        if self.title.trim().is_empty() || self.title.len() > MAX_TITLE_BYTES {
            return Err(ScheduledTaskError::InvalidTask(
                "title must contain between 1 and 200 bytes".into(),
            ));
        }
        if self.prompt.trim().is_empty() || self.prompt.len() > MAX_PROMPT_BYTES {
            return Err(ScheduledTaskError::InvalidTask(
                "prompt must contain between 1 and 131072 bytes".into(),
            ));
        }
        if !self.workspace_root.is_absolute() {
            return Err(ScheduledTaskError::InvalidTask(
                "workspace_root must be an absolute path".into(),
            ));
        }
        if !self.binding.is_ready() {
            return Err(ScheduledTaskError::InvalidTask(
                "a connection and model are required".into(),
            ));
        }
        self.schedule.validate()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduledTaskError {
    #[error("scheduled task database error: {0}")]
    Database(#[from] WorkDatabaseError),
    #[error("scheduled task launchd error: {0}")]
    Launchd(#[from] LaunchdError),
    #[error("invalid scheduled task: {0}")]
    InvalidTask(String),
    #[error("invalid schedule: {0}")]
    InvalidSchedule(String),
    #[error("scheduled task '{0}' not found")]
    NotFound(String),
}

#[derive(Clone)]
pub struct ScheduledTaskService {
    database: Arc<WorkDatabase>,
    launchd: LaunchdManager,
}

impl ScheduledTaskService {
    pub fn new(database: Arc<WorkDatabase>, launchd: LaunchdManager) -> Self {
        Self { database, launchd }
    }

    pub fn list(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<Vec<ScheduledTask>, ScheduledTaskError> {
        Ok(self
            .database
            .scheduled_tasks(workspace_root)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
        self.database
            .scheduled_task(id)?
            .map(Into::into)
            .ok_or_else(|| ScheduledTaskError::NotFound(id.to_owned()))
    }

    pub fn save(&self, mut task: ScheduledTask) -> Result<ScheduledTask, ScheduledTaskError> {
        task.title = task.title.trim().to_owned();
        task.prompt = task.prompt.trim().to_owned();
        task.binding.approval_policy = crate::tool::ToolApprovalPolicy::AllowAll;
        task.updated_at = now();
        task.validate()?;
        let stored: WorkScheduledTask = task.clone().into();
        self.database.save_scheduled_task(&stored)?;
        if let Err(error) = self.launchd.sync(&task) {
            tracing::error!(task_id = %task.id, %error, "scheduled task persisted but launchd synchronization failed");
            return Err(error.into());
        }
        Ok(task)
    }

    pub fn delete(&self, id: &str) -> Result<bool, ScheduledTaskError> {
        let task = self.get(id)?;
        self.launchd.remove(&task)?;
        Ok(self.database.delete_scheduled_task(id)?)
    }

    pub fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduledTask, ScheduledTaskError> {
        let mut task = self.get(id)?;
        task.enabled = enabled;
        self.save(task)
    }

    pub fn record_run(
        &self,
        id: &str,
        conversation_id: Option<&str>,
        success: bool,
        error: Option<&str>,
    ) -> Result<(), ScheduledTaskError> {
        self.database
            .record_scheduled_task_run(id, now(), conversation_id, success, error)?;
        Ok(())
    }

    pub fn database(&self) -> &Arc<WorkDatabase> {
        &self.database
    }
}

impl From<WorkScheduledTask> for ScheduledTask {
    fn from(task: WorkScheduledTask) -> Self {
        Self {
            id: task.id,
            title: task.title,
            prompt: task.prompt,
            workspace_root: task.workspace_root,
            project_id: task.project_id,
            binding: task.binding,
            schedule: task.schedule,
            enabled: task.enabled,
            created_at: task.created_at,
            updated_at: task.updated_at,
            last_run_at: task.last_run_at,
            last_run_success: task.last_run_success,
            last_error: task.last_error,
            last_conversation_id: task.last_conversation_id,
        }
    }
}

impl From<ScheduledTask> for WorkScheduledTask {
    fn from(task: ScheduledTask) -> Self {
        Self {
            id: task.id,
            title: task.title,
            prompt: task.prompt,
            workspace_root: task.workspace_root,
            project_id: task.project_id,
            binding: task.binding,
            schedule: task.schedule,
            enabled: task.enabled,
            created_at: task.created_at,
            updated_at: task.updated_at,
            last_run_at: task.last_run_at,
            last_run_success: task.last_run_success,
            last_error: task.last_error,
            last_conversation_id: task.last_conversation_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionId, SessionBinding};

    fn binding() -> SessionBinding {
        SessionBinding {
            connection_id: Some(ConnectionId("connection".into())),
            model_id: Some("model".into()),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_intervals_shorter_than_one_minute() {
        assert!(ScheduledTaskSchedule::Interval { seconds: 59 }
            .validate()
            .is_err());
        assert!(ScheduledTaskSchedule::Interval { seconds: 60 }
            .validate()
            .is_ok());
    }

    #[test]
    fn validates_calendar_ranges() {
        assert!(ScheduledTaskSchedule::Daily {
            hour: 23,
            minute: 59
        }
        .validate()
        .is_ok());
        assert!(ScheduledTaskSchedule::Daily {
            hour: 24,
            minute: 0
        }
        .validate()
        .is_err());
        assert!(ScheduledTaskSchedule::Weekly {
            weekday: 7,
            hour: 0,
            minute: 0
        }
        .validate()
        .is_err());
    }

    #[test]
    fn new_task_is_explicitly_enabled() {
        let task = ScheduledTask::new(
            "Daily review",
            "Review the repository",
            PathBuf::from("/tmp/workspace"),
            None,
            binding(),
            ScheduledTaskSchedule::Daily { hour: 9, minute: 0 },
        );
        assert!(task.enabled);
        assert!(task.validate().is_ok());
    }
}
