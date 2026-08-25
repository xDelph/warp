//! ACPX command construction and its `--format json --json-strict` output parser.
//!
//! ACPX exposes the underlying ACP JSON-RPC messages verbatim. This module deliberately
//! retains that boundary: it converts only the message shapes that Warp renders, leaving
//! process ownership and conversion into UI-specific tool-call models to the caller.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use serde_json::Value;

/// Resolves the ACPX invocation at first use and caches it for the process lifetime.
///
/// Strategy (first hit wins):
/// 1. `acpx` found on augmented PATH → run it directly (it has a `#!/usr/bin/env node` shebang).
/// 2. `bun` found on PATH + `acpx/dist/cli.js` in a known global `node_modules` → `bun <cli.js>`.
/// 3. `node` found on PATH + `acpx/dist/cli.js` in a known global `node_modules` → `node <cli.js>`.
///
/// This replaces the previous hardcoded `/opt/homebrew/lib/node_modules/acpx/dist/cli.js`
/// which only worked on macOS Apple Silicon with Homebrew.
fn resolve_acpx_invocation() -> (String, Vec<String>) {
    static CACHED: OnceLock<(String, Vec<String>)> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            // 1. Try `acpx` directly from PATH — it's an executable script.
            if let Some(path) = crate::ai::acp::path_search::resolve_command("acpx") {
                return (path.to_string_lossy().into_owned(), Vec::new());
            }

            // 2. Try bun + known global node_modules locations.
            let cli_js = find_acpx_cli_js();
            if let Some(cli) = cli_js {
                if let Some(bun) = crate::ai::acp::path_search::resolve_command("bun") {
                    return (bun.to_string_lossy().into_owned(), vec![cli.to_string_lossy().into_owned()]);
                }
                if let Some(node) = crate::ai::acp::path_search::resolve_command("node") {
                    return (node.to_string_lossy().into_owned(), vec![cli.to_string_lossy().into_owned()]);
                }
            }

            // 3. Last-resort fallback: the old hardcoded path (may not exist).
            ("bun".to_string(), vec!["/opt/homebrew/lib/node_modules/acpx/dist/cli.js".to_string()])
        })
        .clone()
}

/// Searches common global `node_modules` locations for `acpx/dist/cli.js`.
fn find_acpx_cli_js() -> Option<PathBuf> {
    let candidates = acpx_cli_js_candidates();
    candidates.into_iter().find(|p| p.is_file())
}

fn acpx_cli_js_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Homebrew locations (macOS).
    paths.push(PathBuf::from("/opt/homebrew/lib/node_modules/acpx/dist/cli.js"));
    paths.push(PathBuf::from("/usr/local/lib/node_modules/acpx/dist/cli.js"));

    // Linux/Unix global npm.
    paths.push(PathBuf::from("/usr/lib/node_modules/acpx/dist/cli.js"));
    paths.push(PathBuf::from("/usr/local/lib/node_modules/acpx/dist/cli.js"));

    // User-local locations.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".npm-global/lib/node_modules/acpx/dist/cli.js"));
        paths.push(home.join(".local/share/npm/lib/node_modules/acpx/dist/cli.js"));
        paths.push(home.join(".bun/install/global/node_modules/acpx/dist/cli.js"));
        paths.push(home.join(".volta/tools/image/packages/acpx/default/lib/node_modules/acpx/dist/cli.js"));
        // npm prefix — most common on user installs.
        paths.push(home.join(".nvm/versions/node").join("latest").join("lib/node_modules/acpx/dist/cli.js"));
    }

    // NVM version manager — scan all node versions.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let nvm_root = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                paths.push(
                    entry
                        .path()
                        .join("lib/node_modules/acpx/dist/cli.js"),
                );
            }
        }
        let fnm_root = home.join(".fnm/node-versions");
        if let Ok(entries) = std::fs::read_dir(&fnm_root) {
            for entry in entries.flatten() {
                paths.push(
                    entry
                        .path()
                        .join("installation/lib/node_modules/acpx/dist/cli.js"),
                );
            }
        }
    }

    paths
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpxPermissionMode {
    /// Auto-approve all tool calls. Use only for read-only or trusted contexts.
    ApproveAll,
    /// Auto-approve read/search tool calls; deny writes when interactive
    /// prompting is unavailable. Safer default for user prompts.
    ApproveReads,
    /// Deny all tool calls. Used for model discovery and status checks.
    DenyAll,
}

impl AcpxPermissionMode {
    fn flags(self) -> Vec<&'static str> {
        match self {
            Self::ApproveAll => vec!["--approve-all"],
            Self::ApproveReads => vec!["--approve-reads", "--non-interactive-permissions", "deny"],
            Self::DenyAll => vec!["--deny-all"],
        }
    }
}

/// An ACPX subprocess invocation. `program` and `args` are ready for
/// `async_process::Command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpxCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

impl AcpxCommand {
    fn base(
        cwd: &Path,
        agent: &str,
        permission_mode: AcpxPermissionMode,
        ttl_seconds: u32,
    ) -> Self {
        let (program, mut args) = resolve_acpx_invocation();
        args.extend([
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--format".to_string(),
            "json".to_string(),
            "--json-strict".to_string(),
        ]);
        args.extend(permission_mode.flags().iter().map(|f| f.to_string()));
        args.extend([
            "--ttl".to_string(),
            ttl_seconds.to_string(),
            agent.to_string(),
        ]);
        Self { program, args }
    }

    /// Start or resume ACPX's named persistent session, scoped by agent and cwd.
    pub(crate) fn ensure_session(
        cwd: &Path,
        agent: &str,
        session: &str,
        permission_mode: AcpxPermissionMode,
        ttl_seconds: u32,
    ) -> Self {
        let mut command = Self::base(cwd, agent, permission_mode, ttl_seconds);
        command.args.extend([
            "sessions".to_string(),
            "ensure".to_string(),
            "--name".to_string(),
            session.to_string(),
        ]);
        command
    }

    /// Send a prompt through a persistent ACPX session.
    pub(crate) fn prompt(
        cwd: &Path,
        agent: &str,
        session: &str,
        prompt: &str,
        permission_mode: AcpxPermissionMode,
        ttl_seconds: u32,
    ) -> Self {
        let mut command = Self::base(cwd, agent, permission_mode, ttl_seconds);
        command.args.extend([
            "-s".to_string(),
            session.to_string(),
            "prompt".to_string(),
            prompt.to_string(),
        ]);
        command
    }

    /// Read ACPX's persisted snapshot for a named session. ACPX records the
    /// models advertised by `session/new` and exposes them as
    /// `availableModels` here, avoiding a second raw ACP client in Warp.
    pub(crate) fn status(
        cwd: &Path,
        agent: &str,
        session: &str,
        permission_mode: AcpxPermissionMode,
        ttl_seconds: u32,
    ) -> Self {
        let mut command = Self::base(cwd, agent, permission_mode, ttl_seconds);
        command
            .args
            .extend(["status".to_string(), "-s".to_string(), session.to_string()]);
        command
    }

    pub(crate) fn cancel(cwd: &Path, agent: &str, session: &str) -> Self {
        let mut command = Self::base(cwd, agent, AcpxPermissionMode::DenyAll, 0);
        command
            .args
            .extend(["cancel".to_string(), "-s".to_string(), session.to_string()]);
        command
    }

    pub(crate) fn set_mode(cwd: &Path, agent: &str, session: &str, mode: &str) -> Self {
        let mut command = Self::base(cwd, agent, AcpxPermissionMode::DenyAll, 0);
        command.args.extend([
            "set-mode".to_string(),
            mode.to_string(),
            "-s".to_string(),
            session.to_string(),
        ]);
        command
    }

    pub(crate) fn set_model(cwd: &Path, agent: &str, session: &str, model: &str) -> Self {
        let mut command = Self::base(cwd, agent, AcpxPermissionMode::DenyAll, 0);
        command.args.extend([
            "set".to_string(),
            "model".to_string(),
            model.to_string(),
            "-s".to_string(),
            session.to_string(),
        ]);
        command
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AcpxFrame {
    SessionUpdate(AcpxSessionUpdate),
    PromptCompleted(AcpxPromptCompleted),
    ProtocolError(AcpxProtocolError),
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AcpxSessionUpdate {
    Text(String),
    Thought(String),
    ToolCall(AcpxToolCall),
    ToolCallUpdate(AcpxToolCallUpdate),
    Status { kind: String, payload: Value },
    Other { kind: String, payload: Value },
}

/// ACPX passes these fields through without normalizing them. Keep the raw values so the
/// existing Warp tool-card mapper can preserve adapter-specific diffs and terminal output.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcpxToolCall {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) kind: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) raw_input: Option<Value>,
    pub(crate) raw_output: Option<Value>,
    pub(crate) content: Option<Value>,
    pub(crate) locations: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcpxToolCallUpdate {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) raw_input: Option<Value>,
    pub(crate) raw_output: Option<Value>,
    pub(crate) content: Option<Value>,
    pub(crate) locations: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcpxPromptCompleted {
    pub(crate) request_id: Value,
    pub(crate) stop_reason: String,
    pub(crate) usage: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcpxProtocolError {
    pub(crate) request_id: Option<Value>,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
}

/// Parses one line from ACPX's strict JSON output. The caller owns newline framing and must
/// correlate [`AcpxPromptCompleted::request_id`] with the prompt request it launched.
pub(crate) fn parse_acpx_frame(line: &str) -> Result<AcpxFrame> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| anyhow!("invalid ACPX JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("ACPX frame must be a JSON object"))?;

    if let Some(error) = object.get("error") {
        let error_object = error
            .as_object()
            .ok_or_else(|| anyhow!("ACPX JSON-RPC error must be an object"))?;
        let message = error_object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("ACPX protocol error")
            .to_string();
        return Ok(AcpxFrame::ProtocolError(AcpxProtocolError {
            request_id: object.get("id").cloned(),
            message,
            data: error_object.get("data").cloned(),
        }));
    }

    if object.get("method").and_then(Value::as_str) == Some("session/update") {
        return parse_session_update(object);
    }

    let Some(result) = object.get("result").and_then(Value::as_object) else {
        return Ok(AcpxFrame::Other);
    };
    let Some(stop_reason) = result.get("stopReason").and_then(Value::as_str) else {
        return Ok(AcpxFrame::Other);
    };
    let request_id = object
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("ACPX prompt completion is missing its JSON-RPC id"))?;
    Ok(AcpxFrame::PromptCompleted(AcpxPromptCompleted {
        request_id,
        stop_reason: stop_reason.to_string(),
        usage: result.get("usage").cloned(),
    }))
}

fn parse_session_update(object: &serde_json::Map<String, Value>) -> Result<AcpxFrame> {
    let update = object
        .get("params")
        .and_then(Value::as_object)
        .and_then(|params| params.get("update"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("ACPX session/update is missing params.update"))?;
    let kind = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ACPX session/update is missing update.sessionUpdate"))?;

    let event = match kind {
        "agent_message_chunk" => {
            let text = text_content(update)?;
            AcpxSessionUpdate::Text(text)
        }
        "agent_thought_chunk" => {
            let text = text_content(update)?;
            AcpxSessionUpdate::Thought(text)
        }
        "tool_call" => AcpxSessionUpdate::ToolCall(parse_tool_call(update)?),
        "tool_call_update" => AcpxSessionUpdate::ToolCallUpdate(parse_tool_call_update(update)?),
        "available_commands_update"
        | "usage_update"
        | "current_mode_update"
        | "config_option_update"
        | "session_info_update"
        | "plan" => AcpxSessionUpdate::Status {
            kind: kind.to_string(),
            payload: Value::Object(update.clone()),
        },
        _ => AcpxSessionUpdate::Other {
            kind: kind.to_string(),
            payload: Value::Object(update.clone()),
        },
    };
    Ok(AcpxFrame::SessionUpdate(event))
}

fn text_content(update: &serde_json::Map<String, Value>) -> Result<String> {
    update
        .get("content")
        .and_then(Value::as_object)
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("ACPX text update is missing content.type=text and content.text"))
}

fn parse_tool_call(update: &serde_json::Map<String, Value>) -> Result<AcpxToolCall> {
    Ok(AcpxToolCall {
        id: required_string(update, "toolCallId")?,
        title: required_string(update, "title")?,
        kind: optional_string(update, "kind"),
        status: optional_string(update, "status"),
        raw_input: update.get("rawInput").cloned(),
        raw_output: update.get("rawOutput").cloned(),
        content: update.get("content").cloned(),
        locations: update.get("locations").cloned(),
    })
}

fn parse_tool_call_update(update: &serde_json::Map<String, Value>) -> Result<AcpxToolCallUpdate> {
    Ok(AcpxToolCallUpdate {
        id: required_string(update, "toolCallId")?,
        title: optional_string(update, "title"),
        kind: optional_string(update, "kind"),
        status: optional_string(update, "status"),
        raw_input: update.get("rawInput").cloned(),
        raw_output: update.get("rawOutput").cloned(),
        content: update.get("content").cloned(),
        locations: update.get("locations").cloned(),
    })
}

fn required_string(update: &serde_json::Map<String, Value>, field: &str) -> Result<String> {
    update
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("ACPX {field} is missing or is not a string"))
}

fn optional_string(update: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    update
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        AcpxCommand, AcpxFrame, AcpxPermissionMode,
        AcpxSessionUpdate, parse_acpx_frame,
    };

    #[test]
    fn prompt_command_uses_strict_json_output() {
        let command = AcpxCommand::prompt(
            Path::new("/workspace"),
            "codex",
            "conversation-1",
            "Explain this change",
            AcpxPermissionMode::ApproveAll,
            0,
        );

        // Program is resolved dynamically; verify args structure instead.
        assert!(!command.program.is_empty());
        assert!(command.args.contains(&"--cwd".to_string()));
        assert!(command.args.contains(&"/workspace".to_string()));
        assert!(command.args.contains(&"--format".to_string()));
        assert!(command.args.contains(&"json".to_string()));
        assert!(command.args.contains(&"--json-strict".to_string()));
        assert!(command.args.contains(&"--approve-all".to_string()));
        assert!(command.args.contains(&"--ttl".to_string()));
        assert!(command.args.contains(&"codex".to_string()));
        assert!(command.args.contains(&"-s".to_string()));
        assert!(command.args.contains(&"conversation-1".to_string()));
        assert!(command.args.contains(&"prompt".to_string()));
        assert!(command.args.contains(&"Explain this change".to_string()));
    }

    #[test]
    fn status_command_reads_a_named_session_snapshot() {
        let command = AcpxCommand::status(
            Path::new("/workspace"),
            "codex",
            "model-discovery",
            AcpxPermissionMode::DenyAll,
            0,
        );

        assert!(command.args.contains(&"--cwd".to_string()));
        assert!(command.args.contains(&"/workspace".to_string()));
        assert!(command.args.contains(&"--deny-all".to_string()));
        assert!(command.args.contains(&"codex".to_string()));
        assert!(command.args.contains(&"status".to_string()));
        assert!(command.args.contains(&"-s".to_string()));
        assert!(command.args.contains(&"model-discovery".to_string()));
    }

    #[test]
    fn approve_reads_mode_emits_correct_flags() {
        let command = AcpxCommand::prompt(
            Path::new("/workspace"),
            "codex",
            "session-1",
            "do something",
            AcpxPermissionMode::ApproveReads,
            0,
        );

        assert!(command.args.contains(&"--approve-reads".to_string()));
        assert!(command.args.contains(&"--non-interactive-permissions".to_string()));
        assert!(command.args.contains(&"deny".to_string()));
        assert!(!command.args.contains(&"--approve-all".to_string()));
    }

    #[test]
    fn parses_text_delta_from_real_acpx_wire_shape() {
        let line = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "P" }
                }
            }
        })
        .to_string();

        assert_eq!(
            parse_acpx_frame(&line).unwrap(),
            AcpxFrame::SessionUpdate(AcpxSessionUpdate::Text("P".to_string()))
        );
    }

    #[test]
    fn parses_tool_updates_without_losing_adapter_payloads() {
        let line = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tool-1",
                    "status": "completed",
                    "rawOutput": { "stdout": "ok" },
                    "content": [{ "type": "content", "content": { "type": "text", "text": "ok" } }]
                }
            }
        })
        .to_string();

        let AcpxFrame::SessionUpdate(AcpxSessionUpdate::ToolCallUpdate(update)) =
            parse_acpx_frame(&line).unwrap()
        else {
            panic!("expected tool call update");
        };
        assert_eq!(update.id, "tool-1");
        assert_eq!(update.status.as_deref(), Some("completed"));
        assert_eq!(update.raw_output, Some(json!({ "stdout": "ok" })));
    }

    #[test]
    fn retains_numeric_json_rpc_id_for_prompt_completion() {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "stopReason": "end_turn", "usage": { "totalTokens": 6 } }
        })
        .to_string();

        let AcpxFrame::PromptCompleted(completed) = parse_acpx_frame(&line).unwrap() else {
            panic!("expected prompt completion");
        };
        assert_eq!(completed.request_id, json!(2));
        assert_eq!(completed.stop_reason, "end_turn");
        assert_eq!(completed.usage, Some(json!({ "totalTokens": 6 })));
    }

    #[test]
    fn converts_json_rpc_errors_into_typed_frames() {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": { "code": -32000, "message": "permission denied", "data": { "kind": "permission" } }
        })
        .to_string();

        let AcpxFrame::ProtocolError(error) = parse_acpx_frame(&line).unwrap() else {
            panic!("expected protocol error");
        };
        assert_eq!(error.request_id, Some(json!(2)));
        assert_eq!(error.message, "permission denied");
        assert_eq!(error.data, Some(json!({ "kind": "permission" })));
    }
}
