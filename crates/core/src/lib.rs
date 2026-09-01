pub mod agent;
pub mod auth;
pub mod compaction;
pub mod config;
pub mod connections;
pub mod integrations;
pub mod memory;
pub mod models;
pub mod observability;
pub mod prompt;
pub mod provider;
pub mod runtime;
pub mod skill;
pub mod storage;
pub mod task;
pub mod tool;

// Compatibility façades for the public API used by the UI and integrations.
// New code should import the domain modules above.
pub use auth::{codex, credentials, github};
pub use connections as connection;
pub use integrations::mcp;
pub use observability::diagnostics;
pub use storage::{session, work, workspace};
