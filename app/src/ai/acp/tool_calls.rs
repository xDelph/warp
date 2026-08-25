use std::path::Path;

use serde_json::Value;

use super::acpx_runner::{AcpxToolCall, AcpxToolCallUpdate};
use crate::ai::agent::local_acp_tool_call::{
    LocalAcpDiff, LocalAcpToolCallMessage, LocalAcpToolCallStatus, LocalAcpToolKind,
};
use crate::ai::agent::{AIAgentText, AIAgentTextSection, AgentOutputText, ProgrammingLanguage};
use crate::terminal::shell::ShellType;

/// Converts the raw ACP tool event relayed by ACPX into Warp's stable tool-card model.
pub(crate) fn message_from_acpx_tool_call(tool_call: AcpxToolCall) -> LocalAcpToolCallMessage {
    let kind = map_acpx_tool_kind(tool_call.kind.as_deref());
    let locations = locations_from_acpx(tool_call.locations.as_ref());
    let diffs = diffs_from_acpx(tool_call.content.as_ref());
    let body = if kind == LocalAcpToolKind::Edit {
        AIAgentText { sections: vec![] }
    } else {
        map_raw_fields_only(
            kind,
            tool_call.raw_input.as_ref(),
            tool_call.raw_output.as_ref(),
        )
    };

    LocalAcpToolCallMessage {
        tool_call_id: tool_call.id,
        title: tool_call.title,
        kind,
        status: map_acpx_tool_status(tool_call.status.as_deref()),
        body,
        diffs,
        locations,
    }
}

/// Applies only fields present in an ACPX `tool_call_update` event.
pub(crate) fn apply_acpx_tool_call_update(
    message: &mut LocalAcpToolCallMessage,
    update: &AcpxToolCallUpdate,
) {
    if let Some(title) = &update.title {
        message.title = title.clone();
    }
    if let Some(kind) = &update.kind {
        message.kind = map_acpx_tool_kind(Some(kind));
    }
    if let Some(status) = &update.status {
        message.status = map_acpx_tool_status(Some(status));
    }
    if let Some(locations) = &update.locations {
        message.locations = locations_from_acpx(Some(locations));
    }
    if let Some(content) = &update.content {
        let diffs = diffs_from_acpx(Some(content));
        if !diffs.is_empty() {
            message.diffs = diffs;
        }
    }
    if update.raw_input.is_some() || update.raw_output.is_some() {
        let body = map_raw_fields_only(
            message.kind,
            update.raw_input.as_ref(),
            update.raw_output.as_ref(),
        );
        if !body.sections.is_empty() {
            message.body = body;
        }
    }
}

fn map_raw_fields_only(
    kind: LocalAcpToolKind,
    raw_input: Option<&serde_json::Value>,
    raw_output: Option<&serde_json::Value>,
) -> AIAgentText {
    match kind {
        LocalAcpToolKind::Execute => body_from_execute_fields(raw_output),
        LocalAcpToolKind::Edit => AIAgentText { sections: vec![] },
        LocalAcpToolKind::Read => body_from_read_raw_output(raw_output, &[]),
        _ => body_from_generic_raw_fields(raw_input, raw_output),
    }
}

fn body_from_execute_output(raw_output: Option<&serde_json::Value>) -> AIAgentText {
    let mut sections = Vec::new();
    if let Some(output) = raw_output.and_then(extract_command_output) {
        sections.push(shell_output_section(output));
    }
    AIAgentText { sections }
}

fn body_from_execute_fields(raw_output: Option<&serde_json::Value>) -> AIAgentText {
    body_from_execute_output(raw_output)
}

fn body_from_read_raw_output(
    raw_output: Option<&serde_json::Value>,
    locations: &[String],
) -> AIAgentText {
    let path = locations.first().map(String::as_str);
    let mut sections = Vec::new();
    if let Some(output) = raw_output.and_then(format_json_value) {
        sections.push(read_output_section(&output, path));
    }
    AIAgentText { sections }
}

fn body_from_generic_raw_fields(
    raw_input: Option<&serde_json::Value>,
    raw_output: Option<&serde_json::Value>,
) -> AIAgentText {
    let mut sections = Vec::new();
    if let Some(input) = raw_input.and_then(format_json_value) {
        sections.push(text_section(format!(
            "**Input**\n\n{}",
            fenced_code_block("json", &input)
        )));
    }
    if let Some(output) = raw_output.and_then(format_json_value) {
        if should_suppress_edit_raw_output(output.as_str()) {
            return AIAgentText { sections };
        }
        sections.push(text_section(format!(
            "**Output**\n\n{}",
            fenced_code_block("json", &output)
        )));
    }
    AIAgentText { sections }
}

fn shell_output_section(text: String) -> AIAgentTextSection {
    code_section(text, Some(shell_language()))
}

fn read_output_section(text: &str, path: Option<&str>) -> AIAgentTextSection {
    code_section(
        text.to_string(),
        path.and_then(|path| {
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| ProgrammingLanguage::from(extension.to_string()))
        }),
    )
}

fn code_section(code: String, language: Option<ProgrammingLanguage>) -> AIAgentTextSection {
    AIAgentTextSection::Code {
        code,
        language,
        source: None,
    }
}

fn text_section(text: String) -> AIAgentTextSection {
    AIAgentTextSection::PlainText {
        text: AgentOutputText::from(text),
    }
}

fn shell_language() -> ProgrammingLanguage {
    ProgrammingLanguage::Shell(ShellType::Bash)
}

fn extract_command_output(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            let mut parts = Vec::new();
            for key in [
                "stdout",
                "stderr",
                "output",
                "result",
                "combinedOutput",
                "text",
                "message",
            ] {
                if let Some(text) = map.get(key).and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            if parts.is_empty() {
                if let Some(nested @ serde_json::Value::Object(_)) = map.get("output") {
                    return extract_command_output(nested);
                }
                if let Some(content_items) = map.get("content").and_then(|v| v.as_array()) {
                    let texts: Vec<String> = content_items
                        .iter()
                        .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
                        .map(str::to_string)
                        .collect();
                    if !texts.is_empty() {
                        return Some(texts.join("\n"));
                    }
                }
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(text) => Some(text.clone()),
                    other => format_json_value(other),
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        other => format_json_value(other),
    }
}

fn should_suppress_edit_raw_output(output: &str) -> bool {
    output.contains("afterFullFileContent") || output.contains("beforeFullFileContent")
}

fn fenced_code_block(language: &str, content: &str) -> String {
    format!("```{language}\n{content}\n```")
}

fn format_json_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Null => None,
        other => serde_json::to_string_pretty(other).ok(),
    }
}

fn map_acpx_tool_kind(kind: Option<&str>) -> LocalAcpToolKind {
    match kind.unwrap_or_default().to_ascii_lowercase().as_str() {
        "read" | "read_file" | "list" => LocalAcpToolKind::Read,
        "edit" | "write" | "write_file" | "apply_patch" => LocalAcpToolKind::Edit,
        "delete" | "remove" => LocalAcpToolKind::Delete,
        "move" | "rename" => LocalAcpToolKind::Move,
        "search" | "grep" | "glob" => LocalAcpToolKind::Search,
        "execute" | "terminal" | "bash" | "command" => LocalAcpToolKind::Execute,
        "think" => LocalAcpToolKind::Think,
        "fetch" | "web" => LocalAcpToolKind::Fetch,
        "switch_mode" => LocalAcpToolKind::SwitchMode,
        _ => LocalAcpToolKind::Other,
    }
}

fn map_acpx_tool_status(status: Option<&str>) -> LocalAcpToolCallStatus {
    match status.unwrap_or_default().to_ascii_lowercase().as_str() {
        "pending" => LocalAcpToolCallStatus::Pending,
        "in_progress" | "inprogress" | "running" => LocalAcpToolCallStatus::InProgress,
        "completed" | "complete" | "success" => LocalAcpToolCallStatus::Completed,
        "failed" | "error" | "cancelled" => LocalAcpToolCallStatus::Failed,
        _ => LocalAcpToolCallStatus::Pending,
    }
}

fn locations_from_acpx(locations: Option<&Value>) -> Vec<String> {
    locations
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|location| {
            location
                .get("path")
                .or_else(|| location.get("uri"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn diffs_from_acpx(content: Option<&Value>) -> Vec<LocalAcpDiff> {
    content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let diff = item.get("diff").unwrap_or(item);
            let path = diff.get("path").and_then(Value::as_str)?;
            let new_text = diff
                .get("newText")
                .or_else(|| diff.get("new_text"))
                .and_then(Value::as_str)?;
            Some(LocalAcpDiff {
                path: path.to_string(),
                old_text: diff
                    .get("oldText")
                    .or_else(|| diff.get("old_text"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                new_text: new_text.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_acpx_execute_tool_call_with_raw_output() {
        let tool_call = AcpxToolCall {
            id: "exec-1".to_string(),
            title: "ls".to_string(),
            kind: Some("execute".to_string()),
            status: Some("completed".to_string()),
            raw_input: Some(json!({ "command": "ls" })),
            raw_output: Some(json!({ "stdout": "file.txt", "stderr": "warning" })),
            content: None,
            locations: None,
        };

        let message = message_from_acpx_tool_call(tool_call);
        assert_eq!(message.tool_call_id, "exec-1");
        assert_eq!(message.kind, LocalAcpToolKind::Execute);
        assert_eq!(message.status, LocalAcpToolCallStatus::Completed);
        assert_eq!(message.body.sections.len(), 1);
        assert!(message.body_plain_text().is_empty());
    }

    #[test]
    fn maps_acpx_edit_diff_into_structured_diff() {
        let tool_call = AcpxToolCall {
            id: "edit-1".to_string(),
            title: "Edit src/foo.rs".to_string(),
            kind: Some("edit".to_string()),
            status: Some("in_progress".to_string()),
            raw_input: None,
            raw_output: None,
            content: Some(json!([{
                "type": "diff",
                "path": "src/foo.rs",
                "oldText": "old content",
                "newText": "new content"
            }])),
            locations: Some(json!([{ "path": "src/foo.rs" }])),
        };

        let message = message_from_acpx_tool_call(tool_call);
        assert!(message.body.sections.is_empty());
        assert_eq!(message.diffs.len(), 1);
        assert_eq!(message.diffs[0].path, "src/foo.rs");
        assert_eq!(message.diffs[0].old_text.as_deref(), Some("old content"));
        assert_eq!(message.diffs[0].new_text, "new content");
        assert_eq!(message.locations, vec!["src/foo.rs"]);
    }

    #[test]
    fn acpx_update_preserves_existing_body_when_only_status_changes() {
        let mut message = message_from_acpx_tool_call(AcpxToolCall {
            id: "exec-2".to_string(),
            title: "ls".to_string(),
            kind: Some("execute".to_string()),
            status: None,
            raw_input: None,
            raw_output: Some(json!({ "stdout": "a" })),
            content: None,
            locations: None,
        });

        apply_acpx_tool_call_update(
            &mut message,
            &AcpxToolCallUpdate {
                id: "exec-2".to_string(),
                title: None,
                kind: None,
                status: Some("completed".to_string()),
                raw_input: None,
                raw_output: None,
                content: None,
                locations: None,
            },
        );

        assert_eq!(message.status, LocalAcpToolCallStatus::Completed);
        assert_eq!(message.body.sections.len(), 1);
    }
}
