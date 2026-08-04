//! Value types describing an RMUX-backed native pane.

/// Whether Warp created the RMUX pane (and therefore owns its lifecycle) or
/// merely attached to a pane that already existed.
///
/// Warp-created panes are killed on [`crate::pane_group::pane::DetachType::Closed`];
/// externally attached panes are only ever detached, never killed, so closing
/// the Warp pane never destroys work happening outside of Warp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmuxPaneOwnership {
    /// Warp created this RMUX session/pane and is responsible for closing it.
    WarpCreated,
    /// Warp attached to a pane that already existed; Warp only detaches.
    ExternallyAttached,
}

impl RmuxPaneOwnership {
    /// Converts from the plain `bool` stored in the always-compiled
    /// [`crate::app_state::RmuxTerminalPaneSnapshot`].
    #[must_use]
    pub fn from_warp_created(warp_created: bool) -> Self {
        if warp_created {
            Self::WarpCreated
        } else {
            Self::ExternallyAttached
        }
    }

    #[must_use]
    pub fn is_warp_created(self) -> bool {
        matches!(self, Self::WarpCreated)
    }
}

/// Caller intent for creating or attaching to an RMUX-backed pane.
///
/// This is an inert DTO: constructing one does not contact the RMUX daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmuxPaneSpec {
    /// The RMUX session name to create-or-reuse.
    pub session_name: String,
    /// Working directory to launch the session in, when creating a new one.
    pub cwd: Option<String>,
    /// Whether Warp should own the resulting pane's lifecycle.
    pub ownership: RmuxPaneOwnership,
    /// Stable RMUX pane id for restoration. None re-attaches to whichever pane
    /// is currently active in the session.
    pub pane_id: Option<u32>,
}

impl RmuxPaneSpec {
    pub fn new(session_name: impl Into<String>, ownership: RmuxPaneOwnership) -> Self {
        Self {
            session_name: session_name.into(),
            cwd: None,
            ownership,
            pane_id: None,
        }
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_pane_id(mut self, pane_id: u32) -> Self {
        self.pane_id = Some(pane_id);
        self
    }
}

impl RmuxPaneSpec {
    /// Builds a create-or-reuse spec that targets the same session as an
    /// existing [`crate::app_state::RmuxTerminalPaneSnapshot`], for
    /// restoring a pane that has no specific `pane_id` recorded.
    #[must_use]
    pub fn from_snapshot(snapshot: &crate::app_state::RmuxTerminalPaneSnapshot) -> Self {
        let mut spec = Self::new(
            snapshot.session_name.clone(),
            RmuxPaneOwnership::from_warp_created(snapshot.warp_created),
        );
        spec.cwd = snapshot.cwd.clone();
        spec.pane_id = snapshot.pane_id;
        spec
    }
}
