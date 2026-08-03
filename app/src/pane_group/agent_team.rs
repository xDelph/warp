//! Creation of Claude-style local-ACP agent teams as a visible pane grid:
//! the focused pane becomes the team lead's conversation and each teammate
//! gets its own split pane running a child conversation linked to the lead
//! via `parent_conversation_id`. Teammates can run locally or over SSH on a
//! remote host (genesis/exodus), in which case the working tree is synced
//! with `repo-sync push <host>` (repo-vps-sync convention: `~/sync/<repo>`).

use std::path::{Path, PathBuf};

use warpui::{SingletonEntity, ViewContext};

use crate::ai::acp::harness_picker::LocalAcpHarnessModel;
use crate::ai::acp::submit_model::LocalAcpRemoteTarget;
use crate::ai::acp::team::{LocalAcpTeam, LocalAcpTeamMember, LocalAcpTeamModel};
use crate::ai::acp::{path_search, registry};
use crate::ai::blocklist::agent_view::AgentViewEntryOrigin;
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::pane_group::{DefaultSessionModeBehavior, Direction, PaneGroup, PaneId};

const MAX_TEAMMATES: usize = 8;

impl PaneGroup {
    /// Creates a local-ACP agent team: the focused pane hosts the lead
    /// conversation, and `teammates` new panes are split off it (first to the
    /// right, then stacked below) each hosting a child conversation on the
    /// currently selected harness. Prompts submitted in the lead conversation
    /// are dispatched to the teammates and their answers collected back.
    pub(crate) fn create_local_acp_agent_team(
        &mut self,
        teammates: usize,
        remote_host: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        let teammates = teammates.clamp(1, MAX_TEAMMATES);

        let Some(lead_pane_id) = self.focused_pane_id(ctx).as_terminal_pane_id() else {
            log::warn!("Cannot create an agent team: the focused pane is not a terminal pane");
            return;
        };
        let Some(lead_view) = self.terminal_view_from_pane_id(lead_pane_id, ctx) else {
            log::warn!("Cannot create an agent team: no terminal view for the focused pane");
            return;
        };
        let lead_terminal_view_id = lead_view.id();

        let (harness, model_id) = LocalAcpHarnessModel::handle(ctx)
            .read(ctx, |model, _ctx| {
                (model.selected_harness(), model.selected_model_id_owned())
            });
        if !registry::is_local_acp_harness(harness) {
            log::warn!("Cannot create an agent team: {harness} does not support local ACP");
            return;
        }

        let cwd = self
            .startup_path_for_new_session(Some(lead_pane_id), ctx)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let remote = remote_host.map(|host| LocalAcpRemoteTarget {
            host,
            cwd: remote_team_cwd(&cwd),
        });
        if let Some(remote) = &remote {
            sync_working_tree_to_remote(&remote.host, &cwd);
        }

        // Lead conversation in the focused pane.
        let lead_conversation_id =
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                let id = history_model.start_new_conversation(
                    lead_terminal_view_id,
                    /*is_autoexecute_override*/ true,
                    /*is_viewing_shared_session*/ false,
                    /*is_cli_agent_transcript*/ false,
                    ctx,
                );
                if let Some(conversation) = history_model.conversation_mut(&id) {
                    conversation.set_fallback_display_title("Team lead".to_string());
                }
                id
            });
        lead_view.update(ctx, |terminal_view, ctx| {
            terminal_view.enter_agent_view(
                None,
                Some(lead_conversation_id),
                AgentViewEntryOrigin::ChildAgent,
                ctx,
            );
        });

        // Teammate panes: first split right of the lead, then stack below.
        let mut members = Vec::with_capacity(teammates);
        let mut split_base: PaneId = lead_pane_id.into();
        for index in 0..teammates {
            let direction = if index == 0 {
                Direction::Right
            } else {
                Direction::Down
            };
            let member_pane_id = self.add_session_with_default_session_mode_behavior(
                direction,
                Some(split_base),
                Some(lead_pane_id),
                None, /* chosen_shell */
                None, /* conversation_restoration */
                DefaultSessionModeBehavior::Ignore,
                ctx,
            );
            split_base = member_pane_id.into();
            let Some(member_view) = self.terminal_view_from_pane_id(member_pane_id, ctx) else {
                log::warn!("Agent team: no terminal view for teammate pane {member_pane_id:?}");
                continue;
            };
            let member_terminal_view_id = member_view.id();
            let name = format!("teammate-{}", index + 1);

            let member_conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.start_new_child_conversation(
                        member_terminal_view_id,
                        name.clone(),
                        lead_conversation_id,
                        Some(harness),
                        ctx,
                    )
                });
            self.child_agent_panes
                .insert(member_conversation_id, member_pane_id.into());
            member_view.update(ctx, |terminal_view, ctx| {
                terminal_view.enter_agent_view(
                    None,
                    Some(member_conversation_id),
                    AgentViewEntryOrigin::ChildAgent,
                    ctx,
                );
            });

            members.push(LocalAcpTeamMember {
                name,
                conversation_id: member_conversation_id,
                terminal_view: member_view,
                harness,
                model_id: model_id.clone(),
                cwd: cwd.clone(),
                remote: remote.clone(),
            });
        }

        if members.is_empty() {
            log::warn!("Agent team: no teammate panes could be created");
            return;
        }

        let member_count = members.len();
        LocalAcpTeamModel::handle(ctx).update(ctx, |team_model, _ctx| {
            team_model.register_team(LocalAcpTeam {
                name: format!("{harness} team"),
                lead_conversation_id,
                members,
            });
        });
        log::info!(
            "Created a local ACP agent team: 1 lead + {member_count} {harness} teammate(s) in {cwd:?}",
        );

        // Refocus the lead pane so the user types the first team prompt there.
        self.focus_pane(lead_pane_id.into(), true, ctx);
    }
}

/// Remote working directory for teammates spawned over SSH, matching the
/// `repo-sync push <host>` convention of mirroring the working tree to
/// `~/sync/<repo>` (repo = basename of the local working directory).
fn remote_team_cwd(local_cwd: &Path) -> String {
    let repo_name = local_cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    format!("~/sync/{repo_name}")
}

/// Best-effort fire-and-forget push of the local working tree to the remote
/// host before its teammates boot. Failures only log — the teammates surface
/// their own errors if the tree is missing.
fn sync_working_tree_to_remote(host: &str, cwd: &Path) {
    let Some(repo_sync) = path_search::resolve_command("repo-sync") else {
        log::warn!(
            "repo-sync not found; skipping working-tree sync to {host} — remote teammates \
             will use whatever tree is already there"
        );
        return;
    };
    match std::process::Command::new(repo_sync)
        .args(["push", host, "--with-git"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => log::info!("Agent team: syncing working tree to {host} in the background"),
        Err(error) => log::warn!("Agent team: failed to launch repo-sync push {host}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_team_cwd_uses_sync_convention() {
        assert_eq!(
            remote_team_cwd(Path::new("/Users/livio/Documents/warp")),
            "~/sync/warp"
        );
    }

    #[test]
    fn remote_team_cwd_falls_back_for_root_and_trailing_slash() {
        assert_eq!(remote_team_cwd(Path::new("/")), "~/sync/workspace");
        assert_eq!(
            remote_team_cwd(Path::new("/home/livio/warp/")),
            "~/sync/warp"
        );
    }
}
