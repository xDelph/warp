use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol as acp;
use anyhow::{Context, Result, anyhow};
use async_process::Command;
use warp_cli::agent::Harness;

use super::connection::Connection;
use super::{path_search, registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAcpModelInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Cache duration for model discovery results (matches acpx's 24h default)
const MODEL_CACHE_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) fn default_models_for_harness(harness: Harness) -> Vec<LocalAcpModelInfo> {
    registry::spec_for_harness(harness)
        .into_iter()
        .flat_map(|spec| spec.default_models.iter())
        .map(|model| LocalAcpModelInfo {
            id: (*model).to_string(),
            name: (*model).to_string(),
        })
        .collect()
}

/// codex-acp (and other local agents) can stall during `initialize`/`session/new` —
/// e.g. while indexing a large workspace or waiting on a slow auth check — and there's
/// no way for us to distinguish that from a genuine hang. Without a bound, a stalled
/// handshake leaves the model picker stuck on "Loading" forever with no error surfaced.
///
/// codex-acp in particular connects every configured MCP server during
/// `session/new`; servers with expired OAuth retry with backoff, which
/// routinely pushes session creation past 20s. Discovery runs in the
/// background and results are cached for 24h, so a generous bound is cheap.
const MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Some agents (e.g. the Antigravity `agy-acp` adapter) don't include the
/// model option in the `session/new` response — it arrives moments later via
/// a `config_option_update` session notification. Discovery waits this long
/// for such an update before concluding the agent exposes no models.
const CONFIG_OPTION_UPDATE_WAIT: Duration = Duration::from_secs(6);

/// Enhanced model discovery that follows official ACP SDK patterns.
/// Uses agent-client-protocol's AcpAgent to spawn agents from the registry.
pub(crate) async fn discover_models_for_harness(
    harness: Harness,
) -> Result<Vec<LocalAcpModelInfo>> {
    log::debug!("discovering ACP models for {harness} (official SDK)");

    // Check cache first if available
    if let Some(cached_models) = super::model_cache::ModelCache::try_get_cached_models(harness) {
        log::debug!(
            "using cached models for {harness}: {} models",
            cached_models.len()
        );
        return Ok(cached_models);
    }

    let result = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to build local ACP model discovery runtime")?;
        let local_set = tokio::task::LocalSet::new();
        runtime.block_on(local_set.run_until(async {
            tokio::time::timeout(
                MODEL_DISCOVERY_TIMEOUT,
                discover_models_on_local_runtime(harness),
            )
            .await
            .map_err(|_| {
                anyhow!("{harness} ACP model discovery timed out after {MODEL_DISCOVERY_TIMEOUT:?}")
            })?
        }))
    })
    .await
    .context("local ACP model discovery runtime task panicked")?;

    match &result {
        Ok(models) => {
            log::debug!("discovered {} ACP models for {harness}", models.len());
            // Cache the results
            if !models.is_empty() {
                super::model_cache::ModelCache::cache_models(harness, models.clone());
            }
        }
        Err(error) => log::debug!("failed to discover ACP models for {harness}: {error:#}"),
    }
    result
}

async fn discover_models_on_local_runtime(harness: Harness) -> Result<Vec<LocalAcpModelInfo>> {
    let spec = registry::spec_for_harness(harness)
        .ok_or_else(|| anyhow!("{harness} is not an ACP agent"))?;
    let program = path_search::resolve_harness_command(harness, spec.command)
        .with_context(|| format!("{harness} ACP command '{}' was not found", spec.command))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

    let mut command = Command::new(program);
    command.args(spec.args);
    command.current_dir(&cwd);
    command.env("PATH", path_search::augmented_path_env());
    for key in registry::removed_process_env_for_harness(harness) {
        command.env_remove(key);
    }
    for (key, value) in registry::process_env_for_harness(harness) {
        command.env(key, value);
    }

    let connection = Connection::spawn(&mut command)?;
    let initialize_result = connection
        .initialize(super::connection::initialize_request())
        .await?;

    if registry::should_auto_authenticate(harness) {
        if let Some(auth_method) = initialize_result.auth_methods.first() {
            connection
                .authenticate(acp::AuthenticateRequest::new(auth_method.id().clone()))
                .await?;
        }
    }

    // Subscribe before session/new so late `config_option_update`
    // notifications (agy-acp) can't be missed.
    let mut session_updates = connection.subscribe_session_updates();
    log::debug!("{harness} discovery: sending session/new");
    let session = connection
        .new_session(acp::NewSessionRequest::new(cwd))
        .await?;
    log::debug!("{harness} discovery: session/new resolved");

    let mut models = models_from_config_options(session.config_options.as_deref());

    // Agents on the newer ACP spec (e.g. `cursor-agent acp`) report models
    // via the session `models` field instead of config options.
    if models.is_empty() {
        models = models_from_session_models(session.models.as_ref());
    }

    // Wait briefly for a config_option_update that carries the model option
    // (the Antigravity adapter delivers it asynchronously after session/new).
    if models.is_empty() {
        models = models_from_config_option_updates(&mut session_updates).await;
    }
    log::debug!("{harness} discovery: closing connection");
    connection.close().await?;
    log::debug!("{harness} discovery: connection closed");

    // An agent that returns config options without a recognizable model option
    // (schema drift, broken auth surfaced as a degraded option set) would
    // otherwise show up as a silent empty list in the picker.
    if models.is_empty() {
        if let Some(options) = session
            .config_options
            .as_deref()
            .filter(|options| !options.is_empty())
        {
            let option_ids = options
                .iter()
                .map(|option| option.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow!(
                "{harness} returned config options ({option_ids}) but none matched a model option"
            ));
        }
    }
    Ok(models)
}

/// Models advertised through the ACP session `models` field
/// (`SessionModelState`), used by agents on the newer spec shape.
pub(crate) fn models_from_session_models(
    models: Option<&acp::SessionModelState>,
) -> Vec<LocalAcpModelInfo> {
    models
        .map(|state| {
            state
                .available_models
                .iter()
                .map(|model| LocalAcpModelInfo {
                    id: model.model_id.to_string(),
                    name: model.name.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Drains session updates for up to [`CONFIG_OPTION_UPDATE_WAIT`], returning
/// the first model list carried by a `config_option_update` notification.
async fn models_from_config_option_updates(
    session_updates: &mut futures::channel::mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> Vec<LocalAcpModelInfo> {
    use futures::StreamExt as _;

    let deadline = tokio::time::Instant::now() + CONFIG_OPTION_UPDATE_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Vec::new();
        }
        match tokio::time::timeout(remaining, session_updates.next()).await {
            Ok(Some(notification)) => {
                if let acp::SessionUpdate::ConfigOptionUpdate(update) = &notification.update {
                    let models = models_from_config_options(Some(&update.config_options));
                    if !models.is_empty() {
                        return models;
                    }
                }
            }
            // Channel closed or timed out with no model-bearing update.
            Ok(None) | Err(_) => return Vec::new(),
        }
    }
}

pub(crate) fn models_from_config_options(
    config_options: Option<&[acp::SessionConfigOption]>,
) -> Vec<LocalAcpModelInfo> {
    let Some(model_config) = model_config_option(config_options) else {
        return Vec::new();
    };
    let acp::SessionConfigKind::Select(select) = &model_config.kind else {
        return Vec::new();
    };
    select_options(&select.options)
        .into_iter()
        .map(|option| LocalAcpModelInfo {
            id: option.value.to_string(),
            name: option.name,
        })
        .collect()
}

pub(crate) fn model_config_option(
    config_options: Option<&[acp::SessionConfigOption]>,
) -> Option<&acp::SessionConfigOption> {
    config_options?.iter().find(|option| {
        // Try category first (new ACP spec)
        option.category == Some(acp::SessionConfigOptionCategory::Model)
        // Fallback: check id/name for "model" (case-insensitive)
        || option.id.to_string().to_ascii_lowercase().contains("model")
        || option.name.to_ascii_lowercase().contains("model")
        // Fallback for agents that use category-less config options
        || option.id.to_string().to_ascii_lowercase() == "models"
        || option.name.to_ascii_lowercase() == "model"
    })
}

fn select_options(
    options: &acp::SessionConfigSelectOptions,
) -> Vec<acp::SessionConfigSelectOption> {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(options) => options.clone(),
        acp::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().cloned())
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn context_window_tokens_for_selection(
    harness: Harness,
    model_id: Option<&str>,
) -> Option<u32> {
    context_window_tokens_for_model(harness, model_id)
}

fn context_window_tokens_for_model(harness: Harness, _model_id: Option<&str>) -> Option<u32> {
    match harness {
        Harness::Claude => Some(200_000),
        Harness::Codex => Some(400_000),
        Harness::Gemini => Some(1_000_000),
        Harness::Cursor => Some(200_000),
        Harness::Devin => Some(200_000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_antigravity_models() {
        if super::super::path_search::resolve_command("agy-acp").is_none() {
            eprintln!("agy-acp not installed, skipping");
            return;
        }
        let models = discover_models_for_harness(Harness::Gemini).await;
        match &models {
            Ok(m) => eprintln!("Antigravity models discovered: {m:?}"),
            Err(e) => eprintln!("Antigravity model discovery error: {e:#}"),
        }
        assert!(
            models.is_ok(),
            "Antigravity ACP handshake failed: {}",
            models.unwrap_err()
        );
        assert!(
            !models.unwrap().is_empty(),
            "Antigravity ACP returned no models (expected config_option_update delivery)"
        );
    }

    #[tokio::test]
    async fn discover_codex_models() {
        if super::super::path_search::resolve_command("codex-acp").is_none() {
            eprintln!("codex-acp not installed, skipping");
            return;
        }
        let models = discover_models_for_harness(Harness::Codex).await;
        match &models {
            Ok(m) => eprintln!("Codex models discovered: {m:?}"),
            Err(e) => eprintln!("Codex model discovery error: {e:#}"),
        }
        assert!(
            models.is_ok(),
            "Codex ACP handshake failed: {}",
            models.unwrap_err()
        );
        assert!(
            !models.unwrap().is_empty(),
            "Codex ACP returned no models from config_options"
        );
    }

    #[tokio::test]
    async fn discover_cursor_models() {
        if super::super::path_search::resolve_command("cursor-agent").is_none() {
            eprintln!("cursor-agent not installed, skipping");
            return;
        }
        let models = discover_models_for_harness(Harness::Cursor).await;
        match &models {
            Ok(m) => eprintln!("Cursor models discovered: {m:?}"),
            Err(e) => eprintln!("Cursor model discovery error: {e:#}"),
        }
        assert!(
            models.is_ok(),
            "Cursor ACP handshake failed: {}",
            models.unwrap_err()
        );
        assert!(
            !models.unwrap().is_empty(),
            "Cursor ACP returned no models (expected session `models` field)"
        );
    }

    #[tokio::test]
    async fn discover_devin_models() {
        if super::super::path_search::resolve_command("devin").is_none() {
            eprintln!("devin not installed, skipping");
            return;
        }
        let models = discover_models_for_harness(Harness::Devin).await;
        match &models {
            Ok(m) => eprintln!("Devin models discovered: {m:?}"),
            Err(e) => eprintln!("Devin model discovery error: {e:#}"),
        }
        assert!(
            models.is_ok(),
            "Devin ACP handshake failed: {}",
            models.unwrap_err()
        );
        assert!(!models.unwrap().is_empty(), "Devin ACP returned no models");
    }
}
