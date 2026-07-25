pub mod scheduler;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDefinition {
    pub id: String,
    pub title: String,
    pub description: String,
    pub context: String,
    pub metadata: HashMap<String, String>,
    pub priority: TaskPriority,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub success: bool,
    pub output: String,
    pub sub_results: Vec<TaskResult>,
    pub tokens_used: u64,
    pub tool_calls: usize,
}

pub type Result<T> = std::result::Result<T, TaskError>;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("Task '{task}' failed: {message}")]
    Failed { task: String, message: String },
    #[error("Task '{task}' cancelled")]
    Cancelled { task: String },
    #[error("Task '{task}' not found")]
    NotFound { task: String },
    #[error("{0}")]
    Other(String),
}
