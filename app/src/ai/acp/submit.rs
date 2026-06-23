use std::path::PathBuf;

use ai::api_keys::ApiKeyManager;
use anyhow::Result;
use warp_cli::agent::Harness;
use warpui::{EntityId, ModelContext, SingletonEntity, View, ViewContext};

use super::submit_model::{LocalAcpSubmitModel, LocalAcpSubmitRequest};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::ResponseStreamId;

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
    let gemini_api_key = (harness == Harness::Gemini)
        .then(|| ApiKeyManager::as_ref(ctx).keys().google.clone())
        .flatten()
        .filter(|key| !key.trim().is_empty());
    LocalAcpSubmitModel::handle(ctx).update(ctx, |model, ctx: &mut ModelContext<_>| {
        model.submit(
            LocalAcpSubmitRequest {
                prompt,
                harness,
                model_id,
                gemini_api_key,
                cwd,
                conversation_id,
                stream_id,
                terminal_view_id,
            },
            ctx,
        );
    });
    Ok(())
}
