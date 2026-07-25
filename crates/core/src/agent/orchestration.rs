use tokio::sync::mpsc;

pub struct SubAgentHandle {
    pub id: String,
    pub task_id: String,
    result_rx: mpsc::Receiver<SubAgentEvent>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl SubAgentHandle {
    pub fn new(
        id: String,
        task_id: String,
        result_rx: mpsc::Receiver<SubAgentEvent>,
        cancel_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            id,
            task_id,
            result_rx,
            cancel_tx,
        }
    }

    pub async fn recv(&mut self) -> Option<SubAgentEvent> {
        self.result_rx.recv().await
    }

    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    pub fn blocking_cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }
}

#[derive(Debug, Clone)]
pub enum SubAgentEvent {
    Thinking(String),
    ToolCall { tool: String, params: serde_json::Value },
    ToolResult { tool: String, result: String },
    PartialOutput(String),
    Completed { output: String },
    Error(String),
}

pub enum SubAgentSpec {
    Agent(crate::agent::Agent),
}

impl SubAgentSpec {
    pub async fn run(self, _cancel_rx: tokio::sync::watch::Receiver<bool>) {}
}
