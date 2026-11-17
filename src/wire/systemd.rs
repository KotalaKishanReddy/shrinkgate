//! Background service manager: on Linux this manages systemd user units;
//! on macOS this manages launchd LaunchAgents. See `wire::service`.

pub use crate::wire::service::*;
