//! Claude-style agent teams on top of the local ACP integration.
//!
//! A team is one **lead** conversation plus N **member** child conversations
//! (linked via `parent_conversation_id`), each member rendered in its own
//! pane and executing on its own dedicated local-ACP worker — locally or over
//! SSH on a remote host. The lead conversation acts as the orchestrator:
//! prompts submitted to it are dispatched to members (all of them, or one
//! targeted with an `@member-name` prefix) and member answers are collected
//! back into the lead's response stream.

use std::collections::HashMap;
use std::path::PathBuf;

use ai::api_keys::ApiKeyManager;
use warp_cli::agent::Harness;
use warpui::{Entity, EntityId, ModelContext, SingletonEntity, View, ViewContext, ViewHandle};

use super::submit_model::{
    LocalAcpRemoteTarget, LocalAcpSubmitModel, LocalAcpSubmitRequest, LocalAcpTeamCollect,
};
use crate::ai::agent::conversation::{AIConversationId, LocalAcpStreamChunk};
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel, ResponseStreamId};
use crate::terminal::TerminalView;

/// Cap on how much of a member's answer is copied into the lead transcript.
const MEMBER_RESULT_MAX_CHARS: usize = 16_000;

#[derive(Clone)]
pub(crate) struct LocalAcpTeamMember {
    pub(crate) name: String,
    pub(crate) conversation_id: AIConversationId,
    pub(crate) terminal_view: ViewHandle<TerminalView>,
    pub(crate) harness: Harness,
    pub(crate) model_id: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) remote: Option<LocalAcpRemoteTarget>,
}

pub(crate) struct LocalAcpTeam {
    pub(crate) name: String,
    pub(crate) lead_conversation_id: AIConversationId,
    pub(crate) members: Vec<LocalAcpTeamMember>,
}

/// One in-flight lead prompt fan-out: the lead stream stays open until every
/// dispatched member reported back.
struct ActiveTeamDispatch {
    lead_conversation_id: AIConversationId,
    lead_terminal_view_id: EntityId,
    pending_members: usize,
}

#[derive(Default)]
pub(crate) struct LocalAcpTeamModel {
    teams_by_lead: HashMap<AIConversationId, LocalAcpTeam>,
    /// In-flight dispatches keyed by the lead prompt's response stream (not
    /// the conversation) so a second lead prompt submitted while the first is
    /// still collecting can't have its member results complete the wrong
    /// stream.
    active_dispatches: HashMap<ResponseStreamId, ActiveTeamDispatch>,
}

impl LocalAcpTeamModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        // Drop team state for any conversation that is removed, deleted, or
        // cleared from its terminal view so teams never accumulate stale
        // members or leak their terminal view handles.
        let history_handle = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_handle, |this, _handle, event, _ctx| {
            this.handle_history_event(event);
        });
        Self::default()
    }

    fn handle_history_event(&mut self, event: &BlocklistAIHistoryEvent) {
        match event {
            BlocklistAIHistoryEvent::RemoveConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::DeletedConversation {
                conversation_id, ..
            } => self.drop_conversation(*conversation_id),
            BlocklistAIHistoryEvent::ClearedConversationsForTerminalSurface {
                cleared_conversation_ids,
                ..
            } => {
                for conversation_id in cleared_conversation_ids {
                    self.drop_conversation(*conversation_id);
                }
            }
            _ => {}
        }
    }

    fn drop_conversation(&mut self, conversation_id: AIConversationId) {
        // A removed lead dissolves the whole team.
        self.teams_by_lead.remove(&conversation_id);
        // A removed member drops out of every team it belongs to.
        for team in self.teams_by_lead.values_mut() {
            team.members
                .retain(|member| member.conversation_id != conversation_id);
        }
        // Dispatches whose lead conversation vanished can never complete.
        self.active_dispatches
            .retain(|_, dispatch| dispatch.lead_conversation_id != conversation_id);
    }

    pub(crate) fn register_team(&mut self, team: LocalAcpTeam) {
        self.teams_by_lead.insert(team.lead_conversation_id, team);
    }

    pub(crate) fn is_team_lead(&self, conversation_id: &AIConversationId) -> bool {
        self.teams_by_lead.contains_key(conversation_id)
    }

    pub(crate) fn team_for_lead(
        &self,
        conversation_id: &AIConversationId,
    ) -> Option<&LocalAcpTeam> {
        self.teams_by_lead.get(conversation_id)
    }

    /// Finds the team member owning `conversation_id`, if any. Used so that
    /// prompts typed directly into a member pane reach the member's own
    /// dedicated agent session (same worker tag, harness, and remote target
    /// as team dispatches) instead of the shared per-`(harness, cwd)` worker.
    pub(crate) fn member_for_conversation(
        &self,
        conversation_id: &AIConversationId,
    ) -> Option<&LocalAcpTeamMember> {
        self.teams_by_lead
            .values()
            .flat_map(|team| team.members.iter())
            .find(|member| member.conversation_id == *conversation_id)
    }

    /// Called by the submit model when a dispatched member finished (or
    /// failed). Appends the member's answer to the lead's response stream and
    /// completes the stream once every member reported back.
    pub(crate) fn complete_member_dispatch(
        &mut self,
        collect: &LocalAcpTeamCollect,
        result: Result<String, String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let section = match result {
            Ok(text) => format!(
                "\n\n### ✅ {}\n{}",
                collect.member_name,
                truncated_member_result(&text)
            ),
            Err(error) => format!("\n\n### ❌ {} failed\n{error}", collect.member_name),
        };
        append_lead_stream_text(
            &collect.lead_stream_id,
            collect.lead_conversation_id,
            collect.lead_terminal_view_id,
            section,
            ctx,
        );

        let Some(dispatch) = self.active_dispatches.get_mut(&collect.lead_stream_id) else {
            // The lead stream was cleaned up (conversation removed) or this
            // member reported after the dispatch already completed — the
            // answer is still appended above, only the completion is skipped.
            return;
        };
        dispatch.pending_members = dispatch.pending_members.saturating_sub(1);
        if dispatch.pending_members > 0 {
            return;
        }
        let Some(dispatch) = self.active_dispatches.remove(&collect.lead_stream_id) else {
            return;
        };
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
            history_model.mark_response_stream_completed_successfully(
                &collect.lead_stream_id,
                collect.lead_conversation_id,
                dispatch.lead_terminal_view_id,
                ctx,
            );
        });
    }
}

impl Entity for LocalAcpTeamModel {
    type Event = ();
}

impl SingletonEntity for LocalAcpTeamModel {}

/// Routes a prompt submitted in a team-lead conversation to the team members.
///
/// Returns `false` when `lead_conversation_id` is not a team lead (the caller
/// should fall back to the regular single-agent submit path). When it returns
/// `true` the lead's response stream is owned by the team dispatch and is
/// completed once all targeted members reported back.
pub(crate) fn try_dispatch_team_prompt<V: View>(
    prompt: &str,
    lead_conversation_id: AIConversationId,
    lead_stream_id: ResponseStreamId,
    lead_terminal_view_id: EntityId,
    ctx: &mut ViewContext<V>,
) -> bool {
    let (target_member, task) = parse_member_target(prompt);
    let Some((team_name, members)) =
        LocalAcpTeamModel::handle(ctx).read(ctx, |team_model, _ctx| {
            team_model.team_for_lead(&lead_conversation_id).map(|team| {
                let members: Vec<LocalAcpTeamMember> = team
                    .members
                    .iter()
                    .filter(|member| {
                        target_member
                            .is_none_or(|target| member.name.eq_ignore_ascii_case(target))
                    })
                    .cloned()
                    .collect();
                (team.name.clone(), members)
            })
        })
    else {
        return false;
    };

    if members.is_empty() {
        let message = match target_member {
            Some(target) => format!("No team member named \"{target}\"."),
            None => "This team has no members.".to_string(),
        };
        append_lead_stream_text(
            &lead_stream_id,
            lead_conversation_id,
            lead_terminal_view_id,
            message,
            ctx,
        );
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
            history_model.mark_response_stream_completed_successfully(
                &lead_stream_id,
                lead_conversation_id,
                lead_terminal_view_id,
                ctx,
            );
        });
        return true;
    }

    let member_names = members
        .iter()
        .map(|member| member.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    append_lead_stream_text(
        &lead_stream_id,
        lead_conversation_id,
        lead_terminal_view_id,
        format!(
            "Dispatching to {} teammate{}: {member_names}…",
            members.len(),
            if members.len() == 1 { "" } else { "s" },
        ),
        ctx,
    );

    let mut dispatched = 0usize;
    let mut dead_members = Vec::new();
    for member in &members {
        // A member whose pane was closed holds a dangling view handle —
        // updating it would panic. Report it and prune it from the team.
        if member.terminal_view.downgrade().upgrade(ctx).is_none() {
            append_lead_stream_text(
                &lead_stream_id,
                lead_conversation_id,
                lead_terminal_view_id,
                format!(
                    "\n\n### ❌ {} failed\nThe member's pane was closed.",
                    member.name
                ),
                ctx,
            );
            dead_members.push(member.conversation_id);
            continue;
        }
        let member_prompt = member_prompt(&team_name, member, task);
        let started = member.terminal_view.update(ctx, |terminal_view, ctx| {
            terminal_view.ai_controller().update(ctx, |controller, ctx| {
                controller.start_local_acp_request_in_conversation(
                    member_prompt.clone(),
                    member.conversation_id,
                    ctx,
                )
            })
        });
        let Some((member_conversation_id, member_stream_id)) = started else {
            append_lead_stream_text(
                &lead_stream_id,
                lead_conversation_id,
                lead_terminal_view_id,
                format!("\n\n### ❌ {} failed\nCould not start the member request.", member.name),
                ctx,
            );
            continue;
        };

        let gemini_api_key = (member.harness == Harness::Gemini)
            .then(|| ApiKeyManager::as_ref(ctx).keys().google.clone())
            .flatten()
            .filter(|key| !key.trim().is_empty());
        let request = LocalAcpSubmitRequest {
            prompt: member_prompt,
            context_primer: None,
            harness: member.harness,
            model_id: member.model_id.clone(),
            gemini_api_key,
            cwd: member.cwd.clone(),
            conversation_id: member_conversation_id,
            stream_id: member_stream_id,
            terminal_view_id: member.terminal_view.id(),
            remote: member.remote.clone(),
            worker_tag: Some(member.conversation_id),
        };
        let collect = LocalAcpTeamCollect {
            member_name: member.name.clone(),
            lead_conversation_id,
            lead_stream_id: lead_stream_id.clone(),
            lead_terminal_view_id,
        };
        LocalAcpSubmitModel::handle(ctx).update(ctx, |submit_model, ctx| {
            submit_model.submit_for_team(request, collect, ctx);
        });
        dispatched += 1;
    }

    if !dead_members.is_empty() {
        LocalAcpTeamModel::handle(ctx).update(ctx, |team_model, _ctx| {
            for conversation_id in &dead_members {
                team_model.drop_conversation(*conversation_id);
            }
        });
    }

    if dispatched == 0 {
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
            history_model.mark_response_stream_completed_successfully(
                &lead_stream_id,
                lead_conversation_id,
                lead_terminal_view_id,
                ctx,
            );
        });
        return true;
    }

    LocalAcpTeamModel::handle(ctx).update(ctx, |team_model, _ctx| {
        team_model.active_dispatches.insert(
            lead_stream_id,
            ActiveTeamDispatch {
                lead_conversation_id,
                lead_terminal_view_id,
                pending_members: dispatched,
            },
        );
    });
    true
}

fn append_lead_stream_text<C>(
    stream_id: &ResponseStreamId,
    conversation_id: AIConversationId,
    terminal_view_id: EntityId,
    text: String,
    ctx: &mut C,
) where
    C: warpui::UpdateModel + warpui::GetSingletonModelHandle,
{
    BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
        history_model.append_local_acp_stream_chunk(
            stream_id,
            conversation_id,
            terminal_view_id,
            LocalAcpStreamChunk::Text(text),
            ctx,
        );
    });
}

/// Splits an optional leading `@member-name` target off a lead prompt.
fn parse_member_target(prompt: &str) -> (Option<&str>, &str) {
    let trimmed = prompt.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return (None, prompt);
    };
    let name_end = rest
        .find(char::is_whitespace)
        .unwrap_or(rest.len());
    let (name, task) = rest.split_at(name_end);
    if name.is_empty() {
        return (None, prompt);
    }
    (Some(name), task.trim_start())
}

fn member_prompt(team_name: &str, member: &LocalAcpTeamMember, task: &str) -> String {
    let location = match &member.remote {
        Some(remote) => format!("{} on host {}", remote.cwd, remote.host),
        None => member.cwd.display().to_string(),
    };
    member_prompt_text(team_name, &member.name, &location, task)
}

/// The role preamble + task text a team member receives. Split out from
/// [`member_prompt`] so tests can build the exact production prompt without a
/// live terminal view.
pub(crate) fn member_prompt_text(
    team_name: &str,
    member_name: &str,
    location: &str,
    task: &str,
) -> String {
    format!(
        "You are \"{member_name}\", a teammate in agent team \"{team_name}\", working in \
         {location}. The team lead dispatched the task below to you. Work autonomously and end \
         your response with a concise report of what you did and found.\n\n{task}"
    )
}

fn truncated_member_result(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(no text response)".to_string();
    }
    if trimmed.chars().count() <= MEMBER_RESULT_MAX_CHARS {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(MEMBER_RESULT_MAX_CHARS).collect();
    format!("{truncated}\n\n… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_member_target_splits_leading_mention() {
        assert_eq!(
            parse_member_target("@alice fix the tests"),
            (Some("alice"), "fix the tests")
        );
    }

    #[test]
    fn parse_member_target_without_mention_targets_everyone() {
        assert_eq!(
            parse_member_target("fix the tests"),
            (None, "fix the tests")
        );
    }

    #[test]
    fn parse_member_target_bare_at_sign_is_not_a_target() {
        assert_eq!(parse_member_target("@ hello"), (None, "@ hello"));
    }

    #[test]
    fn truncated_member_result_handles_empty_and_long_text() {
        assert_eq!(truncated_member_result("  "), "(no text response)");
        let long = "x".repeat(MEMBER_RESULT_MAX_CHARS + 10);
        assert!(truncated_member_result(&long).ends_with("… (truncated)"));
    }
}
