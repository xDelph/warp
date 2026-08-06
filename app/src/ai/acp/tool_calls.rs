use std::path::Path;

use agent_client_protocol as acp;

use crate::ai::agent::local_acp_tool_call::{
    LocalAcpDiff, LocalAcpToolCallMessage, LocalAcpToolCallStatus, LocalAcpToolKind,
};
use crate::ai::agent::{AIAgentText, AIAgentTextSection, AgentOutputText, ProgrammingLanguage};
use crate::terminal::shell::ShellType;

pub(crate) fn message_from_tool_call(tool_call: acp::ToolCall) -> LocalAcpToolCallMessage {
    let kind = map_tool_kind(tool_call.kind);
    let (body, diffs) = map_tool_call_fields(
        kind,
        &tool_call.content,
        tool_call.raw_input.as_ref(),
        tool_call.raw_output.as_ref(),
        &tool_call.title,
        &tool_call
            .locations
            .iter()
            .map(|location| location.path.display().to_string())
            .collect::<Vec<_>>(),
    );

    LocalAcpToolCallMessage {
        tool_call_id: tool_call.tool_call_id.to_string(),
        title: tool_call.title,
        kind,
        status: map_tool_status(tool_call.status),
        body,
        diffs,
        locations: tool_call
            .locations
            .iter()
            .map(|location| location.path.display().to_string())
            .collect(),
    }
}

pub(crate) fn apply_tool_call_update(
    message: &mut LocalAcpToolCallMessage,
    update: &acp::ToolCallUpdate,
) {
    let fields = &update.fields;
    if let Some(title) = &fields.title {
        message.title = title.clone();
    }
    if let Some(kind) = fields.kind {
        message.kind = map_tool_kind(kind);
    }
    if let Some(status) = fields.status {
        message.status = map_tool_status(status);
    }
    if let Some(locations) = &fields.locations {
        message.locations = locations
            .iter()
            .map(|location| location.path.display().to_string())
            .collect();
    }

    if let Some(content) = &fields.content {
        let (body, diffs) = map_tool_call_fields(
            message.kind,
            content,
            fields.raw_input.as_ref(),
            fields.raw_output.as_ref(),
            &message.title,
            &message.locations,
        );
        if !body.sections.is_empty() {
            message.body = body;
        }
        if !diffs.is_empty() {
            message.diffs = diffs;
        }
    } else if fields.raw_output.is_some() || fields.raw_input.is_some() {
        let body = map_raw_fields_only(
            message.kind,
            fields.raw_input.as_ref(),
            fields.raw_output.as_ref(),
        );
        if !body.sections.is_empty() {
            message.body = body;
        }
    }
}

fn map_tool_call_fields(
    kind: LocalAcpToolKind,
    content: &[acp::ToolCallContent],
    raw_input: Option<&serde_json::Value>,
    raw_output: Option<&serde_json::Value>,
    title: &str,
    locations: &[String],
) -> (AIAgentText, Vec<LocalAcpDiff>) {
    if !content.is_empty() {
        return map_content(kind, content, raw_output, title, locations);
    }

    (map_raw_fields_only(kind, raw_input, raw_output), Vec::new())
}

fn map_content(
    kind: LocalAcpToolKind,
    content: &[acp::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
    title: &str,
    locations: &[String],
) -> (AIAgentText, Vec<LocalAcpDiff>) {
    match kind {
        LocalAcpToolKind::Edit => (
            AIAgentText { sections: vec![] },
            diffs_from_content(content),
        ),
        LocalAcpToolKind::Execute => (
            body_from_execute(content, raw_output, Some(title)),
            Vec::new(),
        ),
        LocalAcpToolKind::Read => (
            body_from_read_content(content, raw_output, locations),
            Vec::new(),
        ),
        _ => (body_from_generic_content(content, raw_output), Vec::new()),
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

fn body_from_execute(
    content: &[acp::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
    title: Option<&str>,
) -> AIAgentText {
    if let Some(output) = raw_output.and_then(extract_command_output) {
        return AIAgentText {
            sections: vec![shell_output_section(output)],
        };
    }

    let mut sections = Vec::new();
    for item in content {
        let acp::ToolCallContent::Content(content) = item else {
            continue;
        };
        let acp::ContentBlock::Text(text) = &content.content else {
            continue;
        };
        if text.text.is_empty() {
            continue;
        }
        if title.is_some_and(|title| text_matches_command_hint(&text.text, title)) {
            continue;
        }
        sections.push(shell_output_section(text.text.clone()));
    }
    AIAgentText { sections }
}

fn text_matches_command_hint(text: &str, title: &str) -> bool {
    let text = text.trim();
    let title = title.trim();
    text == title || title.ends_with(text) || text.ends_with(title)
}

fn body_from_execute_fields(raw_output: Option<&serde_json::Value>) -> AIAgentText {
    body_from_execute_output(raw_output)
}

fn body_from_read_content(
    content: &[acp::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
    locations: &[String],
) -> AIAgentText {
    let path = file_path_from_content_or_locations(content, locations);
    let mut sections = Vec::new();

    for item in content {
        if let acp::ToolCallContent::Content(content) = item {
            if let acp::ContentBlock::Text(text) = &content.content {
                if !text.text.is_empty() {
                    sections.push(read_output_section(&text.text, path.as_deref()));
                }
            }
        }
    }

    if sections.is_empty() {
        return body_from_read_raw_output(raw_output, locations);
    }

    AIAgentText { sections }
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

fn body_from_generic_content(
    content: &[acp::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
) -> AIAgentText {
    let mut sections = Vec::new();
    for item in content {
        match item {
            acp::ToolCallContent::Content(content) => match &content.content {
                acp::ContentBlock::Text(text) if !text.text.is_empty() => {
                    sections.push(text_section(format_tool_body_text(text.text.clone())));
                }
                _ => {}
            },
            acp::ToolCallContent::Terminal(_) => {}
            acp::ToolCallContent::Diff(_) => {}
            _ => {}
        }
    }

    if sections.is_empty() {
        return body_from_generic_raw_fields(None, raw_output);
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

fn diffs_from_content(content: &[acp::ToolCallContent]) -> Vec<LocalAcpDiff> {
    content
        .iter()
        .filter_map(|item| {
            let acp::ToolCallContent::Diff(diff) = item else {
                return None;
            };
            let path = diff.path.display().to_string();
            Some(LocalAcpDiff {
                path,
                old_text: diff.old_text.clone(),
                new_text: diff.new_text.clone(),
            })
        })
        .collect()
}

fn file_path_from_content_or_locations(
    content: &[acp::ToolCallContent],
    locations: &[String],
) -> Option<String> {
    for item in content {
        if let acp::ToolCallContent::Diff(diff) = item {
            return Some(diff.path.display().to_string());
        }
    }
    locations.first().cloned()
}

fn read_output_section(text: &str, path: Option<&str>) -> AIAgentTextSection {
    code_section(text.to_string(), language_for_path(path))
}

fn shell_output_section(text: String) -> AIAgentTextSection {
    code_section(text, Some(shell_language()))
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

fn language_for_path(path: Option<&str>) -> Option<ProgrammingLanguage> {
    path.and_then(|path| {
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ProgrammingLanguage::from(ext.to_string()))
    })
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

fn format_tool_body_text(text: String) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        return text;
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return fenced_code_block("json", trimmed);
    }
    text
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

fn map_tool_kind(kind: acp::ToolKind) -> LocalAcpToolKind {
    match kind {
        acp::ToolKind::Read => LocalAcpToolKind::Read,
        acp::ToolKind::Edit => LocalAcpToolKind::Edit,
        acp::ToolKind::Delete => LocalAcpToolKind::Delete,
        acp::ToolKind::Move => LocalAcpToolKind::Move,
        acp::ToolKind::Search => LocalAcpToolKind::Search,
        acp::ToolKind::Execute => LocalAcpToolKind::Execute,
        acp::ToolKind::Think => LocalAcpToolKind::Think,
        acp::ToolKind::Fetch => LocalAcpToolKind::Fetch,
        acp::ToolKind::SwitchMode => LocalAcpToolKind::SwitchMode,
        acp::ToolKind::Other => LocalAcpToolKind::Other,
        _ => LocalAcpToolKind::Other,
    }
}

fn map_tool_status(status: acp::ToolCallStatus) -> LocalAcpToolCallStatus {
    match status {
        acp::ToolCallStatus::Pending => LocalAcpToolCallStatus::Pending,
        acp::ToolCallStatus::InProgress => LocalAcpToolCallStatus::InProgress,
        acp::ToolCallStatus::Completed => LocalAcpToolCallStatus::Completed,
        acp::ToolCallStatus::Failed => LocalAcpToolCallStatus::Failed,
        _ => LocalAcpToolCallStatus::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent::AIAgentTextSection;

    #[test]
    fn maps_tool_call_with_text_content() {
        let tool_call = acp::ToolCall::new("tool-1", "Read file")
            .kind(acp::ToolKind::Read)
            .status(acp::ToolCallStatus::Completed)
            .content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                acp::TextContent::new("hello"),
            ))]);

        let message = message_from_tool_call(tool_call);
        assert_eq!(message.tool_call_id, "tool-1");
        assert_eq!(message.title, "Read file");
        assert_eq!(message.kind, LocalAcpToolKind::Read);
        assert_eq!(message.status, LocalAcpToolCallStatus::Completed);
        assert!(message.has_visible_body());
    }

    #[test]
    fn wraps_json_tool_output_in_code_fence_for_generic_tools() {
        let tool_call =
            acp::ToolCall::new("tool-2", "grep").content(vec![acp::ToolCallContent::from(
                acp::ContentBlock::Text(acp::TextContent::new(r#"{"success":true}"#)),
            )]);

        let message = message_from_tool_call(tool_call);
        assert!(message.body_plain_text().contains("```json"));
        assert!(message.body_plain_text().contains(r#""success":true"#));
    }

    #[test]
    fn execute_with_content_and_raw_output_shows_stdout_once() {
        let tool_call = acp::ToolCall::new("exec-1", "cd /tmp && ls -al")
            .kind(acp::ToolKind::Execute)
            .content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                acp::TextContent::new("cd /tmp && ls -al"),
            ))])
            .raw_output(serde_json::json!({
                "stdout": "total 0\ndrwxr-xr-x",
                "stderr": ""
            }));

        let message = message_from_tool_call(tool_call);
        assert_eq!(message.body.sections.len(), 1);
        match &message.body.sections[0] {
            AIAgentTextSection::Code { code, language, .. } => {
                assert!(code.contains("total 0"));
                assert!(language.as_ref().is_some_and(|lang| lang.is_shell()));
            }
            other => panic!("expected code section, got {other:?}"),
        }
        assert!(!message.body_plain_text().contains("cd /tmp"));
    }

    #[test]
    fn execute_with_only_raw_output_json_shows_fenced_stdout() {
        let tool_call = acp::ToolCall::new("exec-2", "ls")
            .kind(acp::ToolKind::Execute)
            .raw_output(serde_json::json!({
                "stdout": "file.txt",
                "stderr": "warning"
            }));

        let message = message_from_tool_call(tool_call);
        match &message.body.sections[0] {
            AIAgentTextSection::Code { code, .. } => {
                assert!(code.contains("file.txt"));
                assert!(code.contains("warning"));
            }
            other => panic!("expected code section, got {other:?}"),
        }
    }

    #[test]
    fn read_file_uses_extension_for_syntax_highlighting() {
        let tool_call = acp::ToolCall::new("read-1", "Read src/foo.rs")
            .kind(acp::ToolKind::Read)
            .locations(vec![acp::ToolCallLocation::new("src/foo.rs")])
            .content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                acp::TextContent::new("fn main() {}"),
            ))]);

        let message = message_from_tool_call(tool_call);
        match &message.body.sections[0] {
            AIAgentTextSection::Code { code, language, .. } => {
                assert_eq!(code, "fn main() {}");
                assert!(language.as_ref().is_some_and(|lang| {
                    matches!(lang, ProgrammingLanguage::Other(name) if name == "rs")
                }));
            }
            other => panic!("expected code section, got {other:?}"),
        }
    }

    #[test]
    fn edit_tool_puts_diffs_in_structured_field_not_body() {
        let tool_call = acp::ToolCall::new("edit-1", "Edit src/foo.rs")
            .kind(acp::ToolKind::Edit)
            .content(vec![acp::ToolCallContent::Diff(
                acp::Diff::new("src/foo.rs", "new content").old_text("old content"),
            )])
            .raw_output(serde_json::json!({
                "success": true,
                "afterFullFileContent": "ignored"
            }));

        let message = message_from_tool_call(tool_call);
        assert!(message.body.sections.is_empty());
        assert_eq!(message.diffs.len(), 1);
        assert_eq!(message.diffs[0].path, "src/foo.rs");
        assert_eq!(message.diffs[0].new_text, "new content");
        assert!(message.has_visible_body());
    }

    #[test]
    fn partial_update_does_not_clear_body_on_empty_sections() {
        let mut message = message_from_tool_call(
            acp::ToolCall::new("exec-3", "ls")
                .kind(acp::ToolKind::Execute)
                .raw_output(serde_json::json!({"stdout": "a"})),
        );

        apply_tool_call_update(
            &mut message,
            &acp::ToolCallUpdate::new(
                "exec-3",
                acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed),
            ),
        );

        assert_eq!(message.body.sections.len(), 1);
    }

    #[test]
    fn edit_diff_only_update_sets_diffs_without_body() {
        let mut message = message_from_tool_call(
            acp::ToolCall::new("edit-2", "Edit file")
                .kind(acp::ToolKind::Edit)
                .content(vec![acp::ToolCallContent::Diff(
                    acp::Diff::new("a.txt", "b").old_text("a"),
                )]),
        );

        apply_tool_call_update(
            &mut message,
            &acp::ToolCallUpdate::new(
                "edit-2",
                acp::ToolCallUpdateFields::new().content(vec![acp::ToolCallContent::Diff(
                    acp::Diff::new("a.txt", "bb").old_text("a"),
                )]),
            ),
        );

        assert!(message.body.sections.is_empty());
        assert_eq!(message.diffs[0].new_text, "bb");
    }
}
