//! Local durable state. Secrets deliberately live in `auth/credentials` and
//! never in these stores.

pub mod session;
pub mod work;
pub mod workspace;
