//! Cross-harness context handoff for local ACP conversations.
//!
//! The conversation transcript persisted by Warp is harness-agnostic, but each
//! ACP harness (claude, codex, cursor, devin, …) runs its own agent process
//! with its own session state. When the user switches harness mid-conversation
//! (or resumes a restored conversation whose transcript the current harness
//! never saw), the new agent would otherwise start with zero context.
//!
//! This module builds a compact "context primer" from the in-memory
//! transcript — user messages verbatim, agent output truncated, tool calls as
//! one-liners, plus the set of files touched — capped at [`MAX_PRIMER_CHARS`],
//! eliding the oldest exchanges first. The primer is prepended to the first
//! prompt sent to the new harness's session; the conversation UI keeps showing
//! only the user's actual prompt.

use warp_cli::agent::Harness;

use crate::ai::agent::conversation::AIConversation;
use crate::ai::agent::local_acp_tool_call::{LocalAcpToolCallMessage, LocalAcpToolCallStatus};
use crate::ai::agent::{
    AIAgentExchange, AIAgentOutputMessageType, AIAgentText, AIAgentTextSection,
};

/// Upper bound for the primer text prepended to the outgoing prompt.
pub(crate) const MAX_PRIMER_CHARS: usize = 30_000;
/// Upper bound for agent prose quoted per exchange before truncation.
const MAX_AGENT_TEXT_CHARS_PER_EXCHANGE: usize = 1_500;
/// Upper bound for a single user message quoted in the primer.
const MAX_USER_TEXT_CHARS: usize = 4_000;
const MAX_FILES_TOUCHED: usize = 50;

/// A digest of one completed exchange, ready to be assembled into the primer.
struct ExchangeDigest {
    text: String,
    files_touched: Vec<String>,
}

/// Builds the context primer for a conversation that is being continued by
/// `new_harness`. `previous_harness` is the harness that produced the prior
/// transcript when known (it is unknown after an app restart, where the
/// in-memory harness tracking is empty but the transcript was restored from
/// the database).
///
/// `current_prompt` is the prompt the user just submitted; the exchange
/// created for it (already appended to the conversation, output still pending)
/// is excluded from the digest so the prompt isn't duplicated.
///
/// Returns `None` when there is no prior transcript worth handing off.
pub(crate) fn build_context_primer(
    conversation: &AIConversation,
    previous_harness: Option<Harness>,
    new_harness: Harness,
    current_prompt: &str,
) -> Option<String> {
    build_primer_from_exchanges(
        &conversation.all_exchanges(),
        previous_harness,
        new_harness,
        current_prompt,
    )
}

fn build_primer_from_exchanges(
    exchanges: &[&AIAgentExchange],
    previous_harness: Option<Harness>,
    new_harness: Harness,
    current_prompt: &str,
) -> Option<String> {
    let digests: Vec<ExchangeDigest> = exchanges
        .iter()
        .copied()
        .filter(|exchange| !is_pending_current_exchange(exchange, current_prompt))
        .filter_map(digest_exchange)
        .collect();

    if digests.is_empty() {
        return None;
    }

    let previous_agent = previous_harness
        .map(Harness::display_name)
        .unwrap_or("a previous agent session");
    let header = format!(
        "<conversation-handoff>\n\
         This is an ongoing conversation in the Warp terminal that was previously handled \
         by {previous_agent} and is now continuing with you ({new_agent}). The digest below \
         is the authoritative history of the conversation so far. Continue seamlessly: do \
         not re-introduce yourself, do not repeat completed work, and answer follow-up \
         questions using this history.\n",
        new_agent = new_harness.display_name(),
    );
    let footer_close = "</conversation-handoff>";

    let mut files_touched: Vec<String> = Vec::new();
    for digest in &digests {
        for file in &digest.files_touched {
            if !files_touched.contains(file) {
                files_touched.push(file.clone());
            }
        }
    }
    files_touched.truncate(MAX_FILES_TOUCHED);
    let files_section = if files_touched.is_empty() {
        String::new()
    } else {
        format!("\nFiles touched so far:\n{}\n", files_touched.join("\n"))
    };

    // Keep the newest exchanges within budget, eliding the oldest first.
    let budget = MAX_PRIMER_CHARS
        .saturating_sub(header.len())
        .saturating_sub(files_section.len())
        .saturating_sub(footer_close.len() + 128);
    let truncated_newest;
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for digest in digests.iter().rev() {
        let cost = digest.text.len() + 1;
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(&digest.text);
    }
    if kept.is_empty() {
        // Even the newest digest alone exceeds the budget: keep a truncated
        // version of it rather than handing off nothing.
        let newest = digests.last().expect("digests is non-empty");
        truncated_newest = truncate_on_char_boundary(&newest.text, budget);
        kept.push(&truncated_newest);
    }
    let elided = digests.len() - kept.len();
    kept.reverse();

    let mut primer = header;
    if elided > 0 {
        primer.push_str(&format!(
            "\n[{elided} earlier exchange(s) elided to fit the context budget]\n"
        ));
    }
    for block in kept {
        primer.push('\n');
        primer.push_str(block);
        primer.push('\n');
    }
    primer.push_str(&files_section);
    primer.push_str(footer_close);

    if primer.len() > MAX_PRIMER_CHARS {
        primer = truncate_on_char_boundary(&primer, MAX_PRIMER_CHARS - footer_close.len() - 2);
        primer.push('\n');
        primer.push_str(footer_close);
    }

    Some(primer)
}

/// The transcript marker shown in the conversation UI when a handoff happens.
/// Ends with a sentence terminator + blank line so the incoming agent text
/// renders as its own paragraph.
pub(crate) fn handoff_marker(previous_harness: Option<Harness>, new_harness: Harness) -> String {
    match previous_harness {
        Some(previous) => format!(
            "*Continued from {} — conversation context was handed off to {}.*\n\n",
            previous.display_name(),
            new_harness.display_name(),
        ),
        None => format!(
            "*Continued from a previous session — conversation context was handed off to {}.*\n\n",
            new_harness.display_name(),
        ),
    }
}

/// Whether this exchange is the one just created for the prompt currently
/// being submitted (input matches, no output yet).
fn is_pending_current_exchange(exchange: &AIAgentExchange, current_prompt: &str) -> bool {
    exchange.output_status.output().is_none()
        && exchange.format_input_for_copy() == current_prompt
}

fn digest_exchange(exchange: &AIAgentExchange) -> Option<ExchangeDigest> {
    let user_text = exchange.format_input_for_copy();
    let mut agent_text = String::new();
    let mut tool_lines: Vec<String> = Vec::new();
    let mut files_touched: Vec<String> = Vec::new();

    if let Some(output) = exchange.output_status.output() {
        for message in &output.get().messages {
            match &message.message {
                AIAgentOutputMessageType::Text(text) => {
                    let plain = agent_text_plain(text);
                    if !plain.trim().is_empty() {
                        if !agent_text.is_empty() {
                            agent_text.push('\n');
                        }
                        agent_text.push_str(plain.trim());
                    }
                }
                AIAgentOutputMessageType::LocalAcpToolCall(tool_call) => {
                    tool_lines.push(tool_call_line(tool_call));
                    collect_tool_call_files(tool_call, &mut files_touched);
                }
                _ => {}
            }
        }
    }

    if user_text.trim().is_empty() && agent_text.is_empty() && tool_lines.is_empty() {
        return None;
    }

    let mut text = String::new();
    if !user_text.trim().is_empty() {
        text.push_str("USER:\n");
        text.push_str(&truncate_on_char_boundary(
            user_text.trim(),
            MAX_USER_TEXT_CHARS,
        ));
    }
    if !tool_lines.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("AGENT ACTIONS:\n");
        text.push_str(&tool_lines.join("\n"));
    }
    if !agent_text.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("AGENT:\n");
        text.push_str(&truncate_on_char_boundary(
            &agent_text,
            MAX_AGENT_TEXT_CHARS_PER_EXCHANGE,
        ));
    }

    Some(ExchangeDigest {
        text,
        files_touched,
    })
}

fn tool_call_line(tool_call: &LocalAcpToolCallMessage) -> String {
    let status = match tool_call.status {
        LocalAcpToolCallStatus::Pending | LocalAcpToolCallStatus::InProgress => "unfinished",
        LocalAcpToolCallStatus::Completed => "done",
        LocalAcpToolCallStatus::Failed => "failed",
    };
    let mut line = format!("- {} [{status}]", tool_call.title.trim());
    if !tool_call.locations.is_empty() {
        line.push_str(" (");
        line.push_str(&tool_call.locations.join(", "));
        line.push(')');
    }
    line
}

fn collect_tool_call_files(tool_call: &LocalAcpToolCallMessage, files: &mut Vec<String>) {
    for diff in &tool_call.diffs {
        if !files.contains(&diff.path) {
            files.push(diff.path.clone());
        }
    }
    for location in &tool_call.locations {
        if !files.contains(location) {
            files.push(location.clone());
        }
    }
}

fn agent_text_plain(text: &AIAgentText) -> String {
    text.sections
        .iter()
        .filter_map(|section| match section {
            AIAgentTextSection::PlainText { text } => Some(text.text().to_string()),
            AIAgentTextSection::Code { code, .. } => Some(format!("```\n{code}\n```")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_on_char_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use chrono::Local;

    use super::*;
    use crate::ai::agent::local_acp_tool_call::{LocalAcpDiff, LocalAcpToolKind};
    use crate::ai::agent::{
        AIAgentContext, AIAgentExchangeId, AIAgentInput, AIAgentOutput, AIAgentOutputMessage,
        AIAgentOutputStatus, AgentOutputText, FinishedAIAgentOutput, MessageId, Shared,
        UserQueryMode,
    };
    use crate::ai::llms::LLMId;

    fn user_query_input(query: &str) -> AIAgentInput {
        AIAgentInput::UserQuery {
            query: query.to_string(),
            context: Arc::<[AIAgentContext]>::from([]),
            static_query_type: None,
            referenced_attachments: HashMap::new(),
            user_query_mode: UserQueryMode::Normal,
            running_command: None,
            intended_agent: None,
        }
    }

    fn text_message(index: usize, text: &str) -> AIAgentOutputMessage {
        AIAgentOutputMessage {
            id: MessageId::new(format!("test-text-{index}")),
            message: AIAgentOutputMessageType::Text(AIAgentText {
                sections: vec![AIAgentTextSection::PlainText {
                    text: AgentOutputText::from(text.to_string()),
                }],
            }),
            citations: vec![],
        }
    }

    fn tool_call_message(index: usize, title: &str, path: &str) -> AIAgentOutputMessage {
        AIAgentOutputMessage {
            id: MessageId::new(format!("test-tool-{index}")),
            message: AIAgentOutputMessageType::LocalAcpToolCall(LocalAcpToolCallMessage {
                tool_call_id: format!("tool-{index}"),
                title: title.to_string(),
                kind: LocalAcpToolKind::Edit,
                status: LocalAcpToolCallStatus::Completed,
                body: AIAgentText { sections: vec![] },
                diffs: vec![LocalAcpDiff {
                    path: path.to_string(),
                    old_text: None,
                    new_text: "new".to_string(),
                }],
                locations: vec![],
            }),
            citations: vec![],
        }
    }

    fn exchange(query: &str, output_messages: Vec<AIAgentOutputMessage>) -> AIAgentExchange {
        let output_status = if output_messages.is_empty() {
            AIAgentOutputStatus::Streaming { output: None }
        } else {
            AIAgentOutputStatus::Finished {
                finished_output: FinishedAIAgentOutput::Success {
                    output: Shared::new(AIAgentOutput {
                        messages: output_messages,
                        citations: vec![],
                        server_output_id: None,
                        api_metadata_bytes: None,
                        suggestions: None,
                        telemetry_events: vec![],
                        model_info: None,
                        request_cost: None,
                    }),
                },
            }
        };
        AIAgentExchange {
            id: AIAgentExchangeId::new(),
            input: vec![user_query_input(query)],
            output_status,
            added_message_ids: HashSet::new(),
            start_time: Local::now(),
            finish_time: None,
            time_to_first_token_ms: None,
            working_directory: None,
            model_id: LLMId::from(""),
            request_cost: None,
            coding_model_id: LLMId::from(""),
            cli_agent_model_id: LLMId::from(""),
            computer_use_model_id: LLMId::from(""),
            response_initiator: None,
        }
    }

    #[test]
    fn primer_contains_user_text_agent_text_tools_and_files() {
        let first = exchange(
            "add a login endpoint",
            vec![
                tool_call_message(0, "Edit src/auth.rs", "src/auth.rs"),
                text_message(0, "Added the login endpoint."),
            ],
        );
        let pending = exchange("what did we discuss?", vec![]);
        let primer = build_primer_from_exchanges(
            &[&first, &pending],
            Some(Harness::Claude),
            Harness::Codex,
            "what did we discuss?",
        )
        .expect("primer built");

        assert!(primer.contains("Claude Code"));
        assert!(primer.contains("Codex"));
        assert!(primer.contains("add a login endpoint"));
        assert!(primer.contains("Added the login endpoint."));
        assert!(primer.contains("- Edit src/auth.rs [done]"));
        assert!(primer.contains("Files touched so far:\nsrc/auth.rs"));
        // The pending prompt must not be duplicated into the digest.
        assert!(!primer.contains("USER:\nwhat did we discuss?"));
    }

    #[test]
    fn no_primer_when_only_the_pending_exchange_exists() {
        let pending = exchange("first ever prompt", vec![]);
        assert!(build_primer_from_exchanges(
            &[&pending],
            None,
            Harness::Codex,
            "first ever prompt",
        )
        .is_none());
    }

    #[test]
    fn primer_is_capped_and_elides_oldest_first() {
        let exchanges: Vec<AIAgentExchange> = (0..40)
            .map(|i| {
                exchange(
                    &format!("question {i} {}", "x".repeat(1_000)),
                    vec![text_message(i, &format!("answer {i} {}", "y".repeat(1_000)))],
                )
            })
            .collect();
        let refs: Vec<&AIAgentExchange> = exchanges.iter().collect();
        let primer =
            build_primer_from_exchanges(&refs, Some(Harness::Claude), Harness::Codex, "unrelated")
                .expect("primer built");

        assert!(primer.len() <= MAX_PRIMER_CHARS);
        assert!(primer.contains("earlier exchange(s) elided"));
        // Newest exchange survives, oldest is elided.
        assert!(primer.contains("question 39"));
        assert!(!primer.contains("question 0 "));
    }

    #[test]
    fn marker_names_both_harnesses() {
        let marker = handoff_marker(Some(Harness::Claude), Harness::Codex);
        assert!(marker.contains("Claude Code"));
        assert!(marker.contains("Codex"));
        assert!(marker.ends_with("\n\n"));
    }
}
