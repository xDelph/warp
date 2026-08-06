use std::path::PathBuf;

use ai::api_keys::ApiKeyManager;
use anyhow::Result;
use warp_cli::agent::Harness;
use warpui::{EntityId, ModelContext, SingletonEntity, View, ViewContext};

use super::session_store::LocalAcpSessionStore;
use super::submit_model::{LocalAcpSubmitModel, LocalAcpSubmitRequest};
use crate::ai::agent::conversation::{AIConversationId, LocalAcpStreamChunk};
use crate::ai::blocklist::{BlocklistAIHistoryModel, ResponseStreamId};

/// Best-effort warm start of the selected harness's agent process + ACP
/// session for the given working directory, so the first prompt after
/// entering agent mode doesn't pay the cold-boot handshake.
pub(crate) fn prewarm_selected_local_acp_agent<V: View>(cwd: PathBuf, ctx: &mut ViewContext<V>) {
    if !crate::ai::local_acp::local_acp_enabled(ctx) {
        return;
    }
    let harness = super::harness_picker::LocalAcpHarnessModel::handle(ctx)
        .update(ctx, |model, _ctx| model.selected_harness());
    LocalAcpSubmitModel::handle(ctx).update(ctx, |model, _ctx: &mut ModelContext<_>| {
        model.prewarm(harness, cwd);
    });
}

pub(crate) fn try_submit_local_acp_query<V: View>(
    prompt: String,
    harness: Harness,
    model_id: Option<String>,
    cwd: PathBuf,
    conversation_id: AIConversationId,
    stream_id: ResponseStreamId,
    terminal_view_id: EntityId,
    ctx: &mut ViewContext<V>,
) -> Result<()> {
    // Prompts typed directly into an agent-team member pane must reach that
    // member's own dedicated agent session, not the shared worker for the
    // currently selected harness.
    let team_member = super::team::LocalAcpTeamModel::handle(ctx).read(ctx, |team_model, _ctx| {
        team_model
            .member_for_conversation(&conversation_id)
            .cloned()
    });
    let (harness, model_id, cwd, remote, worker_tag) = match team_member {
        Some(member) => (
            member.harness,
            member.model_id.clone(),
            member.cwd.clone(),
            member.remote.clone(),
            Some(member.conversation_id),
        ),
        None => (harness, model_id, cwd, None, None),
    };
    let gemini_api_key = (harness == Harness::Gemini)
        .then(|| ApiKeyManager::as_ref(ctx).keys().google.clone())
        .flatten()
        .filter(|key| !key.trim().is_empty());
    let context_primer = build_handoff_primer(
        &prompt,
        harness,
        conversation_id,
        &stream_id,
        terminal_view_id,
        ctx,
    );
    LocalAcpSubmitModel::handle(ctx).update(ctx, |model, ctx: &mut ModelContext<_>| {
        model.submit(
            LocalAcpSubmitRequest {
                prompt,
                context_primer,
                harness,
                model_id,
                gemini_api_key,
                cwd,
                conversation_id,
                stream_id,
                terminal_view_id,
                remote,
                worker_tag,
            },
            ctx,
        );
    });
    Ok(())
}

/// Builds a cross-harness context primer when this prompt continues a
/// conversation whose transcript the selected harness has not seen: either
/// the user switched harness mid-conversation, or the conversation was
/// restored (e.g. after an app restart) and the agent session is fresh.
///
/// Also appends a visible "Continued from …" marker to the conversation
/// transcript so the handoff is evident in the UI.
fn build_handoff_primer<V: View>(
    prompt: &str,
    harness: Harness,
    conversation_id: AIConversationId,
    stream_id: &ResponseStreamId,
    terminal_view_id: EntityId,
    ctx: &mut ViewContext<V>,
) -> Option<String> {
    let previous_harness = LocalAcpSessionStore::handle(ctx)
        .update(ctx, |store, _ctx| store.last_harness(conversation_id));
    if previous_harness == Some(harness) {
        return None;
    }

    let history_model = BlocklistAIHistoryModel::handle(ctx);
    let primer = history_model.update(ctx, |history_model, _ctx| {
        let conversation = history_model.conversation(&conversation_id)?;
        super::context_handoff::build_context_primer(
            conversation,
            previous_harness,
            harness,
            prompt,
        )
    })?;

    let marker = super::context_handoff::handoff_marker(previous_harness, harness);
    history_model.update(ctx, |history_model, ctx| {
        history_model.append_local_acp_stream_chunk(
            stream_id,
            conversation_id,
            terminal_view_id,
            LocalAcpStreamChunk::Text(marker),
            ctx,
        );
    });

    log::info!(
        "Local ACP harness handoff for conversation {conversation_id}: {} → {} ({} chars of primer)",
        previous_harness.map_or("<unknown>", |h| h.display_name()),
        harness.display_name(),
        primer.len(),
    );
    Some(primer)
}
