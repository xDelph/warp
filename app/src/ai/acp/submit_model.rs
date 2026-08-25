use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use async_channel;
use async_process::Command;
use futures::{
    StreamExt,
    io::{AsyncBufReadExt, BufReader},
};
use tokio::sync::{mpsc, oneshot};
use warp_cli::agent::Harness;
use warpui::{Entity, EntityId, ModelContext, SingletonEntity};

use super::acpx_runner::{
    AcpxCommand, AcpxFrame, AcpxPermissionMode, AcpxSessionUpdate, parse_acpx_frame,
};
use super::session_store::LocalAcpSessionStore;
use super::{path_search, registry, tool_calls};
use crate::ai::agent::RenderableAIError;
use crate::ai::agent::conversation::{AIConversationId, LocalAcpStreamChunk};
use crate::ai::agent::local_acp_tool_call::LocalAcpToolCallMessage;
use crate::ai::blocklist::{BlocklistAIHistoryModel, ResponseStreamId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalAcpSubmitModelEvent {
    Submitted,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct LocalAcpSubmitRequest {
    pub(crate) prompt: String,
    /// Transcript digest prepended to `prompt` on the wire when this prompt
    /// continues a conversation the target harness has no session context
    /// for (harness switch mid-conversation, or restored conversation).
    /// Never shown in the conversation UI.
    pub(crate) context_primer: Option<String>,
    pub(crate) harness: Harness,
    pub(crate) model_id: Option<String>,
    pub(crate) gemini_api_key: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) conversation_id: AIConversationId,
    pub(crate) stream_id: ResponseStreamId,
    pub(crate) terminal_view_id: EntityId,
    /// When set, the agent subprocess is spawned over SSH on this host instead
    /// of locally. ACP speaks over stdio, so the transport is transparent.
    pub(crate) remote: Option<LocalAcpRemoteTarget>,
    /// When set, the request runs on a dedicated worker keyed by this tag
    /// instead of the shared `(harness, cwd)` worker. Agent-team members use
    /// their conversation ID here so each member owns its own agent process
    /// and members prompt in parallel instead of queueing on one worker.
    pub(crate) worker_tag: Option<AIConversationId>,
}

/// Remote spawn target for a local-ACP agent. The subprocess becomes
/// `ssh <host> "bash -lc 'cd <cwd> && exec <agent command>'"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LocalAcpRemoteTarget {
    pub(crate) host: String,
    pub(crate) cwd: String,
}

/// Routing info for collecting an agent-team member's response back into the
/// team lead's conversation stream.
#[derive(Debug, Clone)]
pub(crate) struct LocalAcpTeamCollect {
    pub(crate) member_name: String,
    pub(crate) lead_conversation_id: AIConversationId,
    pub(crate) lead_stream_id: ResponseStreamId,
    pub(crate) lead_terminal_view_id: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAcpSubmitResult {
    pub(crate) session_id: String,
    /// Concatenated agent message text for this prompt. Used to report an
    /// agent-team member's answer back to the team lead's conversation.
    pub(crate) final_text: String,
}

enum LocalAcpStreamEvent {
    Text(String),
    Thought(String),
    ToolCall(LocalAcpToolCallMessage),
}

pub(crate) struct LocalAcpSubmitModel {
    active_submission: Option<LocalAcpSubmitRequest>,
    workers: HashMap<LocalAcpWorkerKey, mpsc::UnboundedSender<LocalAcpWorkerRequest>>,
}

impl LocalAcpSubmitModel {
    pub(crate) fn new(_: &mut ModelContext<Self>) -> Self {
        Self {
            active_submission: None,
            workers: HashMap::new(),
        }
    }

    pub(crate) fn submit(&mut self, request: LocalAcpSubmitRequest, ctx: &mut ModelContext<Self>) {
        self.submit_with_collect(request, None, ctx);
    }

    /// Submits like [`Self::submit`], and additionally reports the member's
    /// final answer (or failure) to the [`super::team::LocalAcpTeamModel`] so
    /// it can be collected into the team lead's conversation.
    pub(crate) fn submit_for_team(
        &mut self,
        request: LocalAcpSubmitRequest,
        collect: LocalAcpTeamCollect,
        ctx: &mut ModelContext<Self>,
    ) {
        self.submit_with_collect(request, Some(collect), ctx);
    }

    fn submit_with_collect(
        &mut self,
        request: LocalAcpSubmitRequest,
        collect: Option<LocalAcpTeamCollect>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.active_submission = Some(request.clone());
        let completion_request = request.clone();
        let stream_stream_id = completion_request.stream_id.clone();
        let stream_conversation_id = completion_request.conversation_id;
        let stream_terminal_view_id = completion_request.terminal_view_id;
        let (stream_tx, stream_rx) = async_channel::unbounded();
        let result_rx = self.enqueue_request(request, stream_tx);

        ctx.spawn_stream_local(
            stream_rx,
            move |_, event, ctx| {
                let chunk = match event {
                    LocalAcpStreamEvent::Text(chunk) => LocalAcpStreamChunk::Text(chunk),
                    LocalAcpStreamEvent::Thought(chunk) => LocalAcpStreamChunk::Thought(chunk),
                    LocalAcpStreamEvent::ToolCall(tool_call) => {
                        LocalAcpStreamChunk::ToolCall(tool_call)
                    }
                };
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.append_local_acp_stream_chunk(
                        &stream_stream_id,
                        stream_conversation_id,
                        stream_terminal_view_id,
                        chunk,
                        ctx,
                    );
                });
            },
            |_, _| {},
        );

        ctx.spawn(
            async move {
                result_rx
                    .await
                    .context("local ACP worker stopped before returning a response")?
            },
            move |me, result, ctx| {
                me.active_submission = None;
                if let Some(collect) = collect {
                    let member_result = match &result {
                        Ok(result) => Ok(result.final_text.clone()),
                        Err(error) => Err(format!("{error:#}")),
                    };
                    super::team::LocalAcpTeamModel::handle(ctx).update(ctx, |team_model, ctx| {
                        team_model.complete_member_dispatch(&collect, member_result, ctx);
                    });
                }
                match result {
                    Ok(result) => {
                        log::info!("Local ACP session {} completed", result.session_id,);
                        LocalAcpSessionStore::handle(ctx).update(ctx, |store, _ctx| {
                            store.set_session_id(
                                completion_request.harness,
                                result.session_id.clone(),
                            );
                            store.set_last_harness(
                                completion_request.conversation_id,
                                completion_request.harness,
                            );
                        });
                        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                            history_model.mark_response_stream_completed_successfully(
                                &completion_request.stream_id,
                                completion_request.conversation_id,
                                completion_request.terminal_view_id,
                                ctx,
                            );
                        });
                        ctx.emit(LocalAcpSubmitModelEvent::Submitted);
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        log::error!("Local ACP submission failed: {message}");
                        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                            history_model.mark_response_stream_completed_with_error(
                                RenderableAIError::Other {
                                    error_message: message.clone(),
                                    will_attempt_resume: false,
                                    waiting_for_network: false,
                                    is_user_error: false,
                                },
                                false,
                                &completion_request.stream_id,
                                completion_request.conversation_id,
                                completion_request.terminal_view_id,
                                ctx,
                            );
                        });
                        ctx.emit(LocalAcpSubmitModelEvent::Failed(message));
                    }
                }
            },
        );
    }

    fn enqueue_request(
        &mut self,
        request: LocalAcpSubmitRequest,
        stream_tx: async_channel::Sender<LocalAcpStreamEvent>,
    ) -> oneshot::Receiver<Result<LocalAcpSubmitResult>> {
        let key = LocalAcpWorkerKey::from_submit_request(&request);
        let (result_tx, result_rx) = oneshot::channel();
        let worker_request = LocalAcpWorkerRequest::Submit(LocalAcpSubmitJob {
            request,
            stream_tx,
            result_tx,
        });

        let sender = self
            .workers
            .entry(key.clone())
            .or_insert_with(spawn_local_acp_worker)
            .clone();

        if let Err(error) = sender.send(worker_request) {
            self.workers.remove(&key);
            let sender = self
                .workers
                .entry(key)
                .or_insert_with(spawn_local_acp_worker)
                .clone();
            let _ = sender.send(error.0);
        }

        result_rx
    }

    /// Boots the agent subprocess and its ACP session ahead of the first
    /// prompt so switching into agent mode doesn't pay the multi-second
    /// spawn → initialize → session/new cold start.
    ///
    /// Best effort: no result is surfaced; a failed warm-up simply falls back
    /// to the cold-boot path on submit.
    pub(crate) fn prewarm(&mut self, harness: Harness, cwd: PathBuf) {
        let key = LocalAcpWorkerKey {
            harness,
            cwd: cwd.clone(),
            remote: None,
            tag: None,
        };
        let sender = self
            .workers
            .entry(key)
            .or_insert_with(spawn_local_acp_worker)
            .clone();
        let _ = sender.send(LocalAcpWorkerRequest::WarmUp { harness, cwd });
    }
}

impl Entity for LocalAcpSubmitModel {
    type Event = LocalAcpSubmitModelEvent;
}

impl SingletonEntity for LocalAcpSubmitModel {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LocalAcpWorkerKey {
    harness: Harness,
    cwd: PathBuf,
    remote: Option<LocalAcpRemoteTarget>,
    /// Dedicated-worker tag (agent-team member conversation ID). `None` for
    /// the shared per-`(harness, cwd)` worker.
    tag: Option<AIConversationId>,
}

impl LocalAcpWorkerKey {
    fn from_submit_request(request: &LocalAcpSubmitRequest) -> Self {
        Self {
            harness: request.harness,
            cwd: request.cwd.clone(),
            remote: request.remote.clone(),
            // ACPX session names are conversation-scoped. Keep the worker
            // scoped the same way so a second conversation never inherits
            // the first conversation's named ACPX session.
            tag: Some(request.worker_tag.unwrap_or(request.conversation_id)),
        }
    }
}

enum LocalAcpWorkerRequest {
    Submit(LocalAcpSubmitJob),
    WarmUp { harness: Harness, cwd: PathBuf },
}

struct LocalAcpSubmitJob {
    request: LocalAcpSubmitRequest,
    stream_tx: async_channel::Sender<LocalAcpStreamEvent>,
    result_tx: oneshot::Sender<Result<LocalAcpSubmitResult>>,
}

struct LocalAcpWorkerSession {
    harness: Harness,
    cwd: PathBuf,
    session_name: String,
    applied_model_id: Option<String>,
}

fn spawn_local_acp_worker() -> mpsc::UnboundedSender<LocalAcpWorkerRequest> {
    let (tx, rx) = mpsc::unbounded_channel();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            log::error!("failed to build local ACP runtime");
            return;
        };
        let local_set = tokio::task::LocalSet::new();
        runtime.block_on(local_set.run_until(run_local_acp_worker(rx)));
    });
    tx
}

async fn run_local_acp_worker(mut rx: mpsc::UnboundedReceiver<LocalAcpWorkerRequest>) {
    let mut session = None;

    while let Some(worker_request) = rx.recv().await {
        let job = match worker_request {
            LocalAcpWorkerRequest::Submit(job) => job,
            LocalAcpWorkerRequest::WarmUp { harness, cwd } => {
                if session.is_none() {
                    match start_local_acp_session(
                        harness,
                        &cwd,
                        None,
                        None,
                        None,
                        format!("warp-{}-prewarm", harness.to_string().to_ascii_lowercase()),
                    )
                    .await
                    {
                        Ok(warm_session) => {
                            log::debug!("prewarmed local ACP session for {harness} in {cwd:?}");
                            session = Some(warm_session);
                        }
                        Err(error) => {
                            log::debug!(
                                "failed to prewarm local ACP session for {harness}: {error:#}"
                            );
                        }
                    }
                }
                continue;
            }
        };
        let result =
            submit_local_acp_query_on_worker(&mut session, job.request, job.stream_tx).await;
        if result.is_err() {
            session.take();
        }
        let _ = job.result_tx.send(result);
    }
}

async fn submit_local_acp_query_on_worker(
    session: &mut Option<LocalAcpWorkerSession>,
    request: LocalAcpSubmitRequest,
    stream_tx: async_channel::Sender<LocalAcpStreamEvent>,
) -> Result<LocalAcpSubmitResult> {
    if session.is_none() {
        *session = Some(
            start_local_acp_session(
                request.harness,
                &request.cwd,
                request.model_id.as_deref(),
                request.gemini_api_key.as_deref(),
                request.remote.as_ref(),
                acpx_session_name(
                    request.harness,
                    request.conversation_id,
                    request.worker_tag.is_some(),
                ),
            )
            .await?,
        );
    }

    let session = session
        .as_mut()
        .expect("local ACP session exists after initialization");
    apply_session_preferences(request.model_id.as_deref(), session).await?;

    prompt_local_acp_session(session, request, stream_tx).await
}

async fn start_local_acp_session(
    harness: Harness,
    cwd: &PathBuf,
    model_id: Option<&str>,
    gemini_api_key: Option<&str>,
    remote: Option<&LocalAcpRemoteTarget>,
    session_name: String,
) -> Result<LocalAcpWorkerSession> {
    if let Some(remote) = remote {
        bail!(
            "Remote ACP agents are not supported by the ACPX runner (requested {} in {})",
            remote.host,
            remote.cwd
        );
    }
    let profile = registry::acpx_profile_for_harness(harness)
        .ok_or_else(|| anyhow!("{harness} does not support local ACPX"))?;
    let command = acpx_command_for_profile(
        AcpxCommand::ensure_session(
            cwd,
            "warp-placeholder-agent",
            &session_name,
            AcpxPermissionMode::ApproveReads,
            300,
        ),
        profile,
    );
    run_acpx_command(command, cwd, harness, gemini_api_key).await?;

    let mut worker_session = LocalAcpWorkerSession {
        harness,
        cwd: cwd.clone(),
        session_name,
        applied_model_id: None,
    };
    apply_session_preferences(model_id, &mut worker_session).await?;

    Ok(worker_session)
}

fn configure_process_env(command: &mut Command, harness: Harness, _gemini_api_key: Option<&str>) {
    command.env("PATH", path_search::augmented_path_env());
    for key in registry::removed_process_env_for_harness(harness) {
        command.env_remove(key);
    }
    for (key, value) in registry::process_env_for_harness(harness) {
        command.env(key, value);
    }
    // Note: the retired Gemini CLI needed GEMINI_API_KEY injected per
    // request; its replacement (Antigravity via agy-acp) manages Google
    // OAuth itself, so no per-request credentials are set anymore.
}

async fn apply_session_preferences(
    model_id: Option<&str>,
    session: &mut LocalAcpWorkerSession,
) -> Result<()> {
    let Some(model_id) = model_id.filter(|model_id| !model_id.is_empty()) else {
        return Ok(());
    };

    if session.applied_model_id.as_deref() == Some(model_id) {
        return Ok(());
    }

    let profile = registry::acpx_profile_for_harness(session.harness)
        .ok_or_else(|| anyhow!("{} does not support local ACPX", session.harness))?;
    let command = acpx_command_for_profile(
        AcpxCommand::set_model(
            &session.cwd,
            "warp-placeholder-agent",
            &session.session_name,
            model_id,
        ),
        profile,
    );
    run_acpx_command(command, &session.cwd, session.harness, None).await?;
    session.applied_model_id = Some(model_id.to_string());

    Ok(())
}

async fn prompt_local_acp_session(
    session: &LocalAcpWorkerSession,
    request: LocalAcpSubmitRequest,
    stream_tx: async_channel::Sender<LocalAcpStreamEvent>,
) -> Result<LocalAcpSubmitResult> {
    let mut tool_calls_by_id: HashMap<String, LocalAcpToolCallMessage> = HashMap::new();
    let mut final_text = String::new();
    let prompt_text = match &request.context_primer {
        Some(primer) => format!("{primer}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    };
    let profile = registry::acpx_profile_for_harness(session.harness)
        .ok_or_else(|| anyhow!("{} does not support local ACPX", session.harness))?;
    let command = acpx_command_for_profile(
        AcpxCommand::prompt(
            &session.cwd,
            "warp-placeholder-agent",
            &session.session_name,
            &prompt_text,
            AcpxPermissionMode::ApproveReads,
            300,
        ),
        profile,
    );
    let mut process = spawn_acpx_command(
        command,
        &session.cwd,
        session.harness,
        request.gemini_api_key.as_deref(),
    )?;
    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ACPX prompt process did not expose stdout"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut prompt_completed = false;

    while let Some(line) = lines.next().await {
        let line = line?;
        match parse_acpx_frame(&line)? {
            AcpxFrame::SessionUpdate(AcpxSessionUpdate::Text(text)) => {
                final_text.push_str(&text);
                let _ = stream_tx.send(LocalAcpStreamEvent::Text(text)).await;
            }
            AcpxFrame::SessionUpdate(AcpxSessionUpdate::Thought(text)) => {
                let _ = stream_tx.send(LocalAcpStreamEvent::Thought(text)).await;
            }
            AcpxFrame::SessionUpdate(AcpxSessionUpdate::ToolCall(tool_call)) => {
                let message = tool_calls::message_from_acpx_tool_call(tool_call);
                tool_calls_by_id.insert(message.tool_call_id.clone(), message.clone());
                let _ = stream_tx.send(LocalAcpStreamEvent::ToolCall(message)).await;
            }
            AcpxFrame::SessionUpdate(AcpxSessionUpdate::ToolCallUpdate(update)) => {
                if let Some(existing) = tool_calls_by_id.get_mut(&update.id) {
                    tool_calls::apply_acpx_tool_call_update(existing, &update);
                    let _ = stream_tx
                        .send(LocalAcpStreamEvent::ToolCall(existing.clone()))
                        .await;
                }
            }
            AcpxFrame::PromptCompleted(_) => prompt_completed = true,
            AcpxFrame::ProtocolError(error) => bail!("ACPX prompt failed: {}", error.message),
            AcpxFrame::SessionUpdate(_) | AcpxFrame::Other => {}
        }
    }

    let status = process
        .status()
        .await
        .context("failed waiting for ACPX prompt")?;
    if !status.success() {
        bail!("ACPX prompt exited with {status}");
    }
    if !prompt_completed {
        bail!("ACPX prompt exited without a prompt completion frame");
    }

    Ok(LocalAcpSubmitResult {
        session_id: session.session_name.clone(),
        final_text,
    })
}

fn acpx_session_name(harness: Harness, conversation_id: AIConversationId, is_team: bool) -> String {
    let tag = if is_team {
        format!("team-{conversation_id}")
    } else {
        format!("conversation-{conversation_id}")
    };
    format!("warp-{}-{tag}", harness.to_string().to_ascii_lowercase())
}

fn acpx_command_for_profile(
    mut command: AcpxCommand,
    profile: registry::AcpxAgentProfile,
) -> AcpxCommand {
    let placeholder = command
        .args
        .iter()
        .position(|argument| argument == "warp-placeholder-agent")
        .expect("ACPX command includes the agent placeholder");
    command
        .args
        .splice(placeholder..=placeholder, profile.command_args());
    command
}

fn spawn_acpx_command(
    command: AcpxCommand,
    cwd: &std::path::Path,
    harness: Harness,
    gemini_api_key: Option<&str>,
) -> Result<async_process::Child> {
    let mut process = Command::new(command.program);
    process
        .args(command.args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    configure_process_env(&mut process, harness, gemini_api_key);
    process.spawn().context("failed to start ACPX")
}

async fn run_acpx_command(
    command: AcpxCommand,
    cwd: &std::path::Path,
    harness: Harness,
    gemini_api_key: Option<&str>,
) -> Result<()> {
    let output = spawn_acpx_command(command, cwd, harness, gemini_api_key)?
        .output()
        .await
        .context("failed waiting for ACPX")?;
    if !output.status.success() {
        bail!("ACPX exited with {}", output.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn worker_keys_with_distinct_tags_get_distinct_workers() {
        let base = LocalAcpWorkerKey {
            harness: Harness::Claude,
            cwd: PathBuf::from("/tmp"),
            remote: None,
            tag: None,
        };
        let member_a = LocalAcpWorkerKey {
            tag: Some(AIConversationId::new()),
            ..base.clone()
        };
        let member_b = LocalAcpWorkerKey {
            tag: Some(AIConversationId::new()),
            ..base.clone()
        };
        assert_ne!(base, member_a);
        assert_ne!(member_a, member_b);
    }

    #[test]
    fn persistent_acpx_session_names_include_harness_and_conversation_tag() {
        let conversation = AIConversationId::new();
        let normal = acpx_session_name(Harness::Codex, conversation, false);
        let team = acpx_session_name(Harness::Codex, conversation, true);

        assert!(normal.starts_with("warp-codex-conversation-"));
        assert!(normal.ends_with(&conversation.to_string()));
        assert!(team.starts_with("warp-codex-team-"));
        assert!(team.ends_with(&conversation.to_string()));
    }

    fn submit_toy_request_to_worker(
        member_name: &'static str,
        task: &'static str,
        cwd: PathBuf,
    ) -> oneshot::Receiver<Result<LocalAcpSubmitResult>> {
        let member_conversation_id = AIConversationId::new();
        let prompt = super::super::team::member_prompt_text(
            "claude team",
            member_name,
            &cwd.display().to_string(),
            task,
        );
        let request = LocalAcpSubmitRequest {
            prompt,
            context_primer: None,
            harness: Harness::Claude,
            model_id: None,
            gemini_api_key: None,
            cwd,
            conversation_id: member_conversation_id,
            stream_id: ResponseStreamId::new_local(),
            terminal_view_id: EntityId::new(),
            remote: None,
            worker_tag: Some(member_conversation_id),
        };
        // Stream events are unused here; the worker ignores send errors from
        // a dropped receiver. The queued job is processed before the worker
        // notices its sender is gone, so dropping `worker` right after the
        // send also makes the worker thread exit cleanly once it answered.
        let (stream_tx, _stream_rx) = async_channel::unbounded();
        let (result_tx, result_rx) = oneshot::channel();
        let worker = spawn_local_acp_worker();
        worker
            .send(LocalAcpWorkerRequest::Submit(LocalAcpSubmitJob {
                request,
                stream_tx,
                result_tx,
            }))
            .expect("worker accepts the submit job");
        result_rx
    }

    /// Real 1-lead + 2-teammate claude team on a toy task, driven through the
    /// production worker loop: two dedicated workers (one per teammate) each
    /// boot their own `claude-agent-acp` subprocess, receive the exact
    /// member prompt the team dispatcher builds, and the "lead" (this test)
    /// collects both final answers. Requires claude-agent-acp + Claude auth;
    /// skips when the binary is not installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn claude_team_lead_collects_results_from_two_teammates() {
        if path_search::resolve_command("claude-agent-acp").is_none() {
            eprintln!("claude-agent-acp not installed, skipping");
            return;
        }
        let cwd = std::env::temp_dir();

        let teammate_1 = submit_toy_request_to_worker(
            "teammate-1",
            "Reply with exactly this token and nothing else: TEAM_ALPHA_OK",
            cwd.clone(),
        );
        let teammate_2 = submit_toy_request_to_worker(
            "teammate-2",
            "What is 2+3? Reply with only the number.",
            cwd,
        );

        let timeout = Duration::from_secs(180);
        let (result_1, result_2) = tokio::join!(
            tokio::time::timeout(timeout, teammate_1),
            tokio::time::timeout(timeout, teammate_2),
        );

        let result_1 = result_1
            .expect("teammate-1 answered within the timeout")
            .expect("teammate-1 worker returned a result")
            .expect("teammate-1 prompt succeeded");
        let result_2 = result_2
            .expect("teammate-2 answered within the timeout")
            .expect("teammate-2 worker returned a result")
            .expect("teammate-2 prompt succeeded");

        eprintln!(
            "teammate-1 ({}): {}",
            result_1.session_id, result_1.final_text
        );
        eprintln!(
            "teammate-2 ({}): {}",
            result_2.session_id, result_2.final_text
        );

        assert_ne!(
            result_1.session_id, result_2.session_id,
            "each teammate runs in its own agent session"
        );
        assert!(
            result_1.final_text.contains("TEAM_ALPHA_OK"),
            "teammate-1 answer missing token: {}",
            result_1.final_text
        );
        assert!(
            result_2.final_text.contains('5'),
            "teammate-2 answer missing '5': {}",
            result_2.final_text
        );
    }
}
