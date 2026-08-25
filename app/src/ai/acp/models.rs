use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use async_process::Command;
use serde::Deserialize;
use warp_cli::agent::Harness;

use super::acpx_runner::{AcpxCommand, AcpxPermissionMode};
use super::{path_search, registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAcpModelInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Returns models explicitly configured for a harness in Warp's ACPX registry.
///
/// ACPX deliberately owns provider configuration and does not expose the raw
/// ACP session metadata that the retired client used for dynamic model
/// discovery. A harness with no registry models uses the provider default, so
/// callers receive an empty list rather than spawning a second ACP process.
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

/// Discovers the models ACPX persisted when it initialized a named session.
/// ACPX's `status` command exposes those models as a compact JSON snapshot.
pub(crate) async fn discover_models_for_harness(
    harness: Harness,
) -> Result<Vec<LocalAcpModelInfo>> {
    let spec = registry::spec_for_harness(harness)
        .ok_or_else(|| anyhow!("{harness} is not an ACPX harness"))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let session = acpx_model_discovery_session_name(harness);

    run_acpx_command(
        command_for_profile(
            AcpxCommand::ensure_session(
                &cwd,
                "warp-placeholder-agent",
                &session,
                AcpxPermissionMode::DenyAll,
                0,
            ),
            spec.acpx_profile,
        ),
        &cwd,
        harness,
    )
    .await?;
    let output = run_acpx_command(
        command_for_profile(
            AcpxCommand::status(
                &cwd,
                "warp-placeholder-agent",
                &session,
                AcpxPermissionMode::DenyAll,
                0,
            ),
            spec.acpx_profile,
        ),
        &cwd,
        harness,
    )
    .await?;
    let models = models_from_acpx_status(&output)?;
    log::debug!(
        "ACPX model discovery for {harness} resolved {} model(s)",
        models.len()
    );
    Ok(models)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcpxStatusSnapshot {
    #[serde(default)]
    available_models: Vec<String>,
}

fn models_from_acpx_status(status: &str) -> Result<Vec<LocalAcpModelInfo>> {
    let snapshot: AcpxStatusSnapshot = serde_json::from_str(status)
        .context("ACPX status did not return a JSON snapshot")?;
    Ok(snapshot
        .available_models
        .into_iter()
        .map(|model| LocalAcpModelInfo {
            id: model.clone(),
            name: model,
        })
        .collect())
}

fn acpx_model_discovery_session_name(harness: Harness) -> String {
    format!("warp-model-discovery-{}", harness.to_string().to_ascii_lowercase())
}

fn command_for_profile(
    mut command: AcpxCommand,
    profile: registry::AcpxAgentProfile,
) -> AcpxCommand {
    let placeholder = command
        .args
        .iter()
        .position(|argument| argument == "warp-placeholder-agent")
        .expect("ACPX model discovery command includes the agent placeholder");
    command
        .args
        .splice(placeholder..=placeholder, profile.command_args());
    command
}

async fn run_acpx_command(
    command: AcpxCommand,
    cwd: &Path,
    harness: Harness,
) -> Result<String> {
    let mut process = Command::new(command.program);
    process
        .args(command.args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("PATH", path_search::augmented_path_env());
    for key in registry::removed_process_env_for_harness(harness) {
        process.env_remove(key);
    }
    for (key, value) in registry::process_env_for_harness(harness) {
        process.env(key, value);
    }

    let output = process
        .output()
        .await
        .context("failed waiting for ACPX model discovery")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ACPX model discovery exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    String::from_utf8(output.stdout).context("ACPX model discovery emitted non-UTF-8 JSON")
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

    #[test]
    fn default_models_are_registry_backed() {
        let models = default_models_for_harness(Harness::Codex);

        assert_eq!(
            models,
            registry::spec_for_harness(Harness::Codex)
                .unwrap()
                .default_models
                .iter()
                .map(|model| LocalAcpModelInfo {
                    id: (*model).to_string(),
                    name: (*model).to_string(),
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_models_from_acpx_status_snapshot() {
        let models = models_from_acpx_status(
            r#"{"action":"status_snapshot","model":"gpt-5.6-terra","availableModels":["gpt-5.6-sol","gpt-5.6-terra"]}"#,
        )
        .unwrap();

        assert_eq!(
            models,
            vec![
                LocalAcpModelInfo {
                    id: "gpt-5.6-sol".to_string(),
                    name: "gpt-5.6-sol".to_string(),
                },
                LocalAcpModelInfo {
                    id: "gpt-5.6-terra".to_string(),
                    name: "gpt-5.6-terra".to_string(),
                },
            ]
        );
    }

    #[test]
    fn missing_acpx_models_remains_a_valid_empty_catalog() {
        assert!(models_from_acpx_status(r#"{"action":"status_snapshot"}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn discovery_session_name_is_stable_per_harness() {
        assert_eq!(
            acpx_model_discovery_session_name(Harness::Codex),
            "warp-model-discovery-codex"
        );
    }

    #[test]
    fn context_windows_remain_harness_specific() {
        assert_eq!(
            context_window_tokens_for_selection(Harness::Codex, None),
            Some(400_000)
        );
        assert_eq!(
            context_window_tokens_for_selection(Harness::Claude, None),
            Some(200_000)
        );
    }
}
