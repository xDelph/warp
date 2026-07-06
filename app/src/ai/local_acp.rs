//! Runtime helpers for the local ACP feature toggle.
//!
//! The `local_acp` Cargo feature controls whether ACP code is compiled into the binary.
//! The user setting (`AISettings::local_acp_enabled`) controls whether local ACP routing is
//! active at runtime when the feature is present.

use std::path::PathBuf;

use warp_cli::agent::Harness;
use warpui::{AppContext, SingletonEntity};

use crate::settings::AISettings;
use crate::workspace::ActiveSession;

#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
use crate::ai::acp::{
    harness_picker::{LocalAcpHarnessModel, LocalAcpHarnessModelEvent},
    submit_model::LocalAcpSubmitModel,
};
#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
use crate::settings::AISettingsChangedEvent;
#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
use ai::api_keys::ApiKeyManager;

/// Returns true when local ACP routing and UI should be active.
pub(crate) fn local_acp_enabled(ctx: &AppContext) -> bool {
    AISettings::as_ref(ctx).is_local_acp_enabled(ctx)
}

/// When true, cloud/Oz agent paths should be suppressed in favor of local ACP.
pub(crate) fn cloud_agent_disabled(ctx: &AppContext) -> bool {
    local_acp_enabled(ctx)
}

/// Pre-spawns the selected harness agent when auto-spawn is enabled, or tears down all workers
/// when local ACP is disabled.
#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
pub(crate) fn sync_auto_spawn_workers(ctx: &mut AppContext) {
    if !local_acp_enabled(ctx) {
        LocalAcpSubmitModel::handle(ctx).update(ctx, |model, _| model.shutdown_all_workers());
        return;
    }

    if !AISettings::as_ref(ctx).is_local_acp_auto_spawn_enabled(ctx) {
        return;
    }

    let cwd = warm_cwd_for_active_window(ctx);
    let harness_model = LocalAcpHarnessModel::as_ref(ctx);
    let harness = harness_model.selected_harness();
    let model_id = harness_model.selected_model_id_owned();
    let gemini_api_key = gemini_api_key_for_harness(harness, ctx);

    LocalAcpSubmitModel::handle(ctx).update(ctx, |model, ctx| {
        model.warm_worker(harness, model_id, gemini_api_key, cwd, ctx);
    });
}

/// Subscribes to app-level events that should trigger ACP pre-spawn. Call once during launch.
#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
pub(crate) fn register_auto_spawn_listeners(ctx: &mut AppContext) {
    ctx.subscribe_to_model(&AISettings::handle(ctx), |_, event, ctx| {
        if matches!(
            event,
            AISettingsChangedEvent::LocalAcpEnabled { .. }
                | AISettingsChangedEvent::LocalAcpAutoSpawnEnabled { .. }
        ) {
            sync_auto_spawn_workers(ctx);
        }
    });

    ctx.subscribe_to_model(&LocalAcpHarnessModel::handle(ctx), |_, event, ctx| {
        if matches!(event, LocalAcpHarnessModelEvent::SelectionChanged) {
            sync_auto_spawn_workers(ctx);
        }
    });

    ctx.subscribe_to_model(&ActiveSession::handle(ctx), |_, _, ctx| {
        if AISettings::as_ref(ctx).is_local_acp_auto_spawn_enabled(ctx) {
            sync_auto_spawn_workers(ctx);
        }
    });
}

#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
pub(crate) fn shutdown_local_acp_workers(ctx: &mut AppContext) {
    LocalAcpSubmitModel::handle(ctx).update(ctx, |model, _| model.shutdown_all_workers());
}

#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
fn warm_cwd_for_active_window(ctx: &AppContext) -> PathBuf {
    ctx.windows()
        .state()
        .active_window
        .and_then(|window_id| ActiveSession::as_ref(ctx).path_if_local(window_id))
        .filter(|path| path.is_dir())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(all(feature = "local_acp", not(target_family = "wasm")))]
fn gemini_api_key_for_harness(harness: Harness, ctx: &AppContext) -> Option<String> {
    (harness == Harness::Gemini)
        .then(|| ApiKeyManager::as_ref(ctx).keys().google.clone())
        .flatten()
        .filter(|key| !key.trim().is_empty())
}

#[cfg(all(test, feature = "local_acp", not(target_family = "wasm")))]
#[path = "local_acp_tests.rs"]
mod tests;
