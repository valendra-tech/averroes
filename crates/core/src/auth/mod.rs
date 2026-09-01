//! Authentication and secret material.
//!
//! Provider protocol implementations live under `provider/`; this module only
//! owns sign-in, token refresh, and encrypted credential storage.

pub mod codex;
pub mod credentials;
pub mod github;
