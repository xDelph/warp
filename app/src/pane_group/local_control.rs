//! Local-control (`warpctrl`) projections of pane-group state.
//!
//! These power the cmux-style agent-bridge verbs: listing sibling panes with
//! enough identity (title, remote host, cwd) to classify which agent lives in
//! each pane, and resolving a caller's own pane from the
//! `WARP_TERMINAL_SESSION_UUID` env var Warp exports into every session.
use serde::Serialize;
use warpui::AppContext;

use super::{PaneGroup, PaneId, TerminalPane};
use crate::pane_group::pane::PaneContent as _;

/// Serializable identity summary for one terminal pane.
#[derive(Debug, Clone, Serialize)]
pub struct ControlPaneSummary {
    /// Stable pane identity (hex), matching `WARP_TERMINAL_SESSION_UUID` in
    /// the pane's shell environment.
    pub pane_uuid: String,
    /// Current pane title (foreground process / OSC title / conversation).
    pub title: String,
    pub is_focused: bool,
    /// Local working directory of the active session, when known and local.
    pub cwd: Option<String>,
    /// Remote hostname when the pane's active session is attached over SSH.
    pub remote_hostname: Option<String>,
}

impl PaneGroup {
    /// Identity summaries for every terminal pane in this group (tab).
    pub fn control_pane_summaries(&self, ctx: &AppContext) -> Vec<ControlPaneSummary> {
        let focused_pane_id = self.focused_pane_id(ctx);
        self.terminal_pane_ids()
            .filter_map(|pane_id| self.control_pane_summary(pane_id, focused_pane_id, ctx))
            .collect()
    }

    /// Identity summary for one pane of this group.
    pub fn control_pane_summary(
        &self,
        pane_id: PaneId,
        focused_pane_id: PaneId,
        ctx: &AppContext,
    ) -> Option<ControlPaneSummary> {
        let terminal_pane = self.downcast_pane_by_id::<TerminalPane>(pane_id)?;
        let terminal_view = terminal_pane.terminal_view(ctx);
        let view = terminal_view.as_ref(ctx);
        Some(ControlPaneSummary {
            pane_uuid: hex::encode(terminal_pane.session_uuid()),
            title: terminal_pane
                .pane_configuration()
                .as_ref(ctx)
                .title()
                .to_owned(),
            is_focused: pane_id == focused_pane_id,
            cwd: view
                .active_session_path_if_local(ctx)
                .map(|path| path.display().to_string()),
            remote_hostname: view.active_remote_session_hostname(ctx),
        })
    }

    /// Hex-encoded stable uuid of a terminal pane (the value the pane's shell
    /// sees as `WARP_TERMINAL_SESSION_UUID`).
    pub fn terminal_pane_uuid_hex(&self, pane_id: PaneId, _ctx: &AppContext) -> Option<String> {
        self.downcast_pane_by_id::<TerminalPane>(pane_id)
            .map(|pane| hex::encode(pane.session_uuid()))
    }

    /// Finds the terminal pane whose identity matches a hex-encoded
    /// `WARP_TERMINAL_SESSION_UUID` value.
    pub fn find_terminal_pane_by_uuid(
        &self,
        pane_uuid_hex: &str,
        _ctx: &AppContext,
    ) -> Option<PaneId> {
        self.terminal_pane_ids().find(|pane_id| {
            self.downcast_pane_by_id::<TerminalPane>(*pane_id)
                .is_some_and(|pane| hex::encode(pane.session_uuid()) == pane_uuid_hex)
        })
    }
}
