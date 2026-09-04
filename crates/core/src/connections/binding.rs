use super::ConnectionId;
use crate::tool::ToolApprovalPolicy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<ConnectionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default)]
    pub approval_policy: ToolApprovalPolicy,
}

impl SessionBinding {
    pub fn is_ready(&self) -> bool {
        self.connection_id.is_some()
            && self
                .model_id
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty())
    }
}
