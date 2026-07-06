use std::path::PathBuf;
use std::time::Duration;

use acpx::RuntimeContext;
use agent_client_protocol as acp;
use anyhow::{anyhow, Context, Result};
use async_process::Command;
use warp_cli::agent::Harness;

use super::connection::Connection;
use super::{path_search, registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAcpModelInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

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
const MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) async fn discover_models_for_harness(
    harness: Harness,
) -> Result<Vec<LocalAcpModelInfo>> {
    if harness == Harness::Gemini {
        return Ok(default_models_for_harness(harness));
    }

    log::debug!("discovering ACP models for {harness}");
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
                anyhow!(
                    "{harness} ACP model discovery timed out after {MODEL_DISCOVERY_TIMEOUT:?}"
                )
            })?
        }))
    })
    .await
    .context("local ACP model discovery runtime task panicked")?;

    match &result {
        Ok(models) => log::debug!("discovered {} ACP models for {harness}", models.len()),
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

    let runtime = RuntimeContext::new(|task| {
        tokio::task::spawn_local(task);
    });
    let connection = Connection::spawn(&mut command, &runtime)?;
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

    let session = connection
        .new_session(acp::NewSessionRequest::new(cwd))
        .await?;
    let models = models_from_config_options(session.config_options.as_deref());
    connection.close().await?;
    Ok(models)
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
    async fn discover_gemini_models_uses_static_defaults() {
        let models = discover_models_for_harness(Harness::Gemini).await.unwrap();
        assert_eq!(models, default_models_for_harness(Harness::Gemini));
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
        if super::super::path_search::resolve_command("cursor-acp").is_none() {
            eprintln!("cursor-acp not installed, skipping");
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
    }
}
