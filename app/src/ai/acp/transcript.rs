//! acpx-compatible transcript formatting for Warp display.
//!
//! Converts Warp's ACP output to match acpx's structured transcript format
//! for better display consistency and integration with acpx tooling.

use serde::{Deserialize, Serialize};
use warp_cli::agent::Harness;

use crate::ai::agent::conversation::LocalAcpStreamChunk;

/// acpx-style transcript entry types that match the acpx output format
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcpxTranscriptEntry {
    #[serde(rename = "acpx.session")]
    Session {
        agent: String,
        mode: String,
        permission_mode: String,
        acp_session_id: String,
        runtime_session_name: String,
    },
    #[serde(rename = "acpx.status")]
    Status {
        tag: String,
        #[serde(flatten)]
        values: serde_json::Value,
    },
    #[serde(rename = "acpx.text_delta")]
    TextDelta {
        text: String,
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    #[serde(rename = "acpx.tool_call")]
    ToolCall {
        name: String,
        tool_call_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    #[serde(rename = "acpx.done")]
    Done {
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Extracts plain text from AIAgentText for transcript display
fn extract_text_from_agent_text(text: &crate::ai::agent::AIAgentText) -> String {
    text.sections
        .iter()
        .filter_map(|section| match section {
            crate::ai::agent::AIAgentTextSection::PlainText { text } => {
                Some(text.text().to_string())
            }
            _ => None,
        })
        .collect()
}

/// Converts Warp's current ACP stream chunks to acpx transcript format
pub fn convert_to_acpx_transcript(
    chunks: &[LocalAcpStreamChunk],
    harness: &Harness,
    session_id: &str,
) -> Vec<AcpxTranscriptEntry> {
    let mut transcript = Vec::new();

    // Add session metadata entry
    transcript.push(AcpxTranscriptEntry::Session {
        agent: harness.display_name().to_string(),
        mode: "persistent".to_string(),
        permission_mode: "approve-all".to_string(),
        acp_session_id: session_id.to_string(),
        runtime_session_name: format!("warp-acpx-{}", harness.display_name().to_lowercase()),
    });

    for chunk in chunks {
        match chunk {
            LocalAcpStreamChunk::Text(text) => {
                transcript.push(AcpxTranscriptEntry::TextDelta {
                    text: text.clone(),
                    channel: "output".to_string(),
                    tag: Some("agent_message_chunk".to_string()),
                });
            }
            LocalAcpStreamChunk::Thought(text) => {
                transcript.push(AcpxTranscriptEntry::TextDelta {
                    text: text.clone(),
                    channel: "thought".to_string(),
                    tag: None,
                });
            }
            LocalAcpStreamChunk::ToolCall(call) => {
                let tool_text = extract_text_from_agent_text(&call.body);
                transcript.push(AcpxTranscriptEntry::ToolCall {
                    name: call.title.clone(),
                    tool_call_id: call.tool_call_id.clone(),
                    status: "running".to_string(),
                    text: if tool_text.is_empty() {
                        None
                    } else {
                        Some(tool_text)
                    },
                    input: None,
                });
            }
        }
    }

    transcript
}

/// Formats acpx transcript entries for human-readable display in Warp
pub fn format_acpx_transcript_for_display(entries: &[AcpxTranscriptEntry]) -> String {
    let mut output = String::new();

    for entry in entries {
        match entry {
            AcpxTranscriptEntry::Session { agent, .. } => {
                output.push_str(&format!("[session] Connected to {} agent\n", agent));
            }
            AcpxTranscriptEntry::Status { tag, values } => {
                output.push_str(&format!("[status] {}: {}\n", tag, values));
            }
            AcpxTranscriptEntry::TextDelta { text, channel, .. } => match channel.as_str() {
                "thought" => output.push_str(&format!("[thinking] {}\n", text)),
                "output" => output.push_str(text),
                _ => output.push_str(text),
            },
            AcpxTranscriptEntry::ToolCall {
                name, status, text, ..
            } => match status.as_str() {
                "running" => {
                    if let Some(t) = text {
                        output.push_str(&format!("[tool] {} {}\n", name, t));
                    } else {
                        output.push_str(&format!("[tool] {} (running)\n", name));
                    }
                }
                "completed" => {
                    output.push_str(&format!("[tool] {} completed\n", name));
                }
                "failed" => {
                    if let Some(t) = text {
                        output.push_str(&format!("[tool] {} failed: {}\n", name, t));
                    } else {
                        output.push_str(&format!("[tool] {} failed\n", name));
                    }
                }
                _ => {}
            },
            AcpxTranscriptEntry::Done { error: None } => {
                output.push_str("[done]\n");
            }
            AcpxTranscriptEntry::Done { error: Some(err) } => {
                output.push_str(&format!("[done] Error: {}\n", err));
            }
        }
    }

    output
}
