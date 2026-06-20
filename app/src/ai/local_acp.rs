//! Runtime helpers for the local ACP feature toggle.
//!
//! The `local_acp` Cargo feature controls whether ACP code is compiled into the binary.
//! The user setting (`AISettings::local_acp_enabled`) controls whether local ACP routing is
//! active at runtime when the feature is present.

use warpui::{AppContext, SingletonEntity};

use crate::settings::AISettings;

/// Returns true when local ACP routing and UI should be active.
pub(crate) fn local_acp_enabled(ctx: &AppContext) -> bool {
    AISettings::as_ref(ctx).is_local_acp_enabled(ctx)
}

/// When true, cloud/Oz agent paths should be suppressed in favor of local ACP.
pub(crate) fn cloud_agent_disabled(ctx: &AppContext) -> bool {
    local_acp_enabled(ctx)
}

#[cfg(all(test, feature = "local_acp", not(target_family = "wasm")))]
#[path = "local_acp_tests.rs"]
mod tests;
