//! Abstraction over the RMUX SDK pane surface, so the terminal manager and
//! event loop can be exercised against a fake client in tests without an
//! external `rmux` daemon.

use std::sync::Arc;

use async_trait::async_trait;
use rmux_sdk::{PaneSnapshot, Result as RmuxResult, TerminalSizeSpec};
use rmux_server::ServerHandle;

use super::input::RmuxInputAction;

/// One render update delivered by [`RmuxPaneClient::next_render_update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmuxRenderUpdate {
    pub snapshot: PaneSnapshot,
    /// Set when the daemon reported lag before this snapshot was captured;
    /// callers should log a warning and treat the snapshot as an
    /// authoritative full resync.
    pub lagged: bool,
}

/// Minimal async surface the RMUX event loop needs from a live pane.
///
/// This mirrors the subset of `rmux_sdk::Pane` that V1 uses: an initial
/// snapshot, a render-update stream, input, resize, and lifecycle teardown.
/// It exists so the event loop can be driven by [`FakeRmuxPaneClient`] in
/// unit tests instead of a real daemon connection.
#[async_trait]
pub trait RmuxPaneClient: Send + Sync {
    /// Captures a fresh full-grid snapshot.
    async fn snapshot(&self) -> RmuxResult<PaneSnapshot>;

    /// Waits for the next render update (debounced output-driven snapshot).
    /// Returns `None` once the pane's output stream has closed for good.
    async fn next_render_update(&self) -> RmuxResult<Option<RmuxRenderUpdate>>;

    /// Forwards one input action to the pane.
    async fn send_input(&self, action: RmuxInputAction) -> RmuxResult<()>;

    /// Requests a pane resize.
    async fn resize(&self, size: TerminalSizeSpec) -> RmuxResult<()>;

    /// Kills the RMUX pane (used for Warp-created panes on close).
    async fn close(&self) -> RmuxResult<()>;

    /// Releases this handle without touching the RMUX pane (used for
    /// externally-attached panes, and for Warp-created panes that are only
    /// being hidden/moved rather than closed).
    fn detach(&self);
}

/// Real [`RmuxPaneClient`] backed by a live `rmux_sdk::Pane` handle.
pub struct RmuxSdkPaneClient {
    pane: rmux_sdk::Pane,
    render_stream: tokio::sync::Mutex<Option<rmux_sdk::PaneRenderStream>>,
    /// Optional daemon handle for Warp-owned panes; keeps embedded daemon alive.
    _daemon_handle: Option<Arc<ServerHandle>>,
}

impl RmuxSdkPaneClient {
    pub fn new(pane: rmux_sdk::Pane) -> Self {
        Self {
            pane,
            render_stream: tokio::sync::Mutex::new(None),
            _daemon_handle: None,
        }
    }

    pub fn with_daemon_handle(pane: rmux_sdk::Pane, daemon_handle: Arc<ServerHandle>) -> Self {
        Self {
            pane,
            render_stream: tokio::sync::Mutex::new(None),
            _daemon_handle: Some(daemon_handle),
        }
    }
}

#[async_trait]
impl RmuxPaneClient for RmuxSdkPaneClient {
    async fn snapshot(&self) -> RmuxResult<PaneSnapshot> {
        self.pane.snapshot().await
    }

    async fn next_render_update(&self) -> RmuxResult<Option<RmuxRenderUpdate>> {
        let mut guard = self.render_stream.lock().await;
        if guard.is_none() {
            *guard = Some(self.pane.render_stream().await?);
        }
        // `guard` was just populated above if empty, so this is infallible.
        let stream = guard.as_mut().expect("render stream initialized above");
        match stream.next().await? {
            Some(update) => Ok(Some(RmuxRenderUpdate {
                lagged: update.lag().is_some(),
                snapshot: update.into_snapshot(),
            })),
            None => Ok(None),
        }
    }

    async fn send_input(&self, action: RmuxInputAction) -> RmuxResult<()> {
        match action {
            RmuxInputAction::Text(text) => self.pane.send_text(text).await,
            RmuxInputAction::Key(key) => self.pane.send_key(key).await,
        }
    }

    async fn resize(&self, size: TerminalSizeSpec) -> RmuxResult<()> {
        self.pane.resize(size).await
    }

    async fn close(&self) -> RmuxResult<()> {
        // `Pane::close` consumes the handle; clone the (cheaply-clonable)
        // handle so `self.pane` remains usable for any in-flight callers.
        self.pane.clone().close().await.map(|_| ())
    }

    fn detach(&self) {
        // `Pane::detach` consumes the handle and is a pure no-op; there is
        // nothing to release on a shared `&self` handle.
    }
}

/// Connects to the RMUX daemon (starting it if necessary), ensures the
/// requested session exists, and returns a client for its first pane.
///
/// This is the only place a fresh RMUX pane is created from a
/// [`super::types::RmuxPaneSpec`]; restoring a previously-known pane id goes
/// through [`connect_rmux_pane_by_id`] instead.
pub async fn connect_rmux_pane(
    spec: super::types::RmuxPaneSpec,
) -> anyhow::Result<(std::sync::Arc<dyn RmuxPaneClient>, Option<u32>)> {
    use anyhow::Context as _;

    let rmux = match spec.ownership {
        super::types::RmuxPaneOwnership::WarpCreated => {
            // For Warp-owned panes, use the separate RMUX daemon for persistence.
            // The daemon stays alive after GUI exit, enabling true tmux-like reattach.
            let socket_path = crate::remote_server::ensure_rmux_daemon_running()
                .context("failed to ensure RMUX daemon is running")?;

            rmux_sdk::Rmux::builder()
                .unix_socket(&socket_path)
                .connect()
                .await
                .context("connect to RMUX daemon via Unix socket")?
        }
        super::types::RmuxPaneOwnership::ExternallyAttached => {
            // For externally-created panes, use the external rmux binary.
            rmux_sdk::Rmux::builder()
                .connect_or_start()
                .await
                .context("connect to rmux daemon (is the `rmux` binary installed?)")?
        }
    };

    let session_name = rmux_sdk::SessionName::new(&spec.session_name)
        .map_err(|error| anyhow::anyhow!("invalid rmux session name: {error}"))?;

    let mut ensure = rmux_sdk::EnsureSession::named(session_name)
        .policy(rmux_sdk::EnsureSessionPolicy::CreateOrReuse)
        .detached(true);
    if let Some(cwd) = spec.cwd {
        ensure = ensure.working_directory(cwd);
    }

    let session = rmux
        .ensure_session(ensure)
        .await
        .context("ensure rmux session")?;

    let pane = match spec.pane_id {
        Some(pane_id) => session
            .pane_by_id(rmux_sdk::PaneId::from(pane_id))
            .await
            .with_context(|| {
                format!(
                    "reattach to rmux pane {pane_id} in session {}",
                    spec.session_name
                )
            })?,
        None => session.pane(0, 0),
    };

    // Capture the pane_id for persistence if this is a new pane
    let captured_pane_id = if spec.pane_id.is_none() {
        pane.id()
            .await
            .context("read RMUX pane id")?
            .map(Into::into)
    } else {
        spec.pane_id
    };

    let client = RmuxSdkPaneClient::new(pane);

    Ok((std::sync::Arc::new(client), captured_pane_id))
}

#[cfg(any(test, feature = "test-util"))]
pub mod fake {
    use std::sync::Mutex;

    use rmux_sdk::PaneCursor;

    use super::*;

    /// One recorded call made through a [`FakeRmuxPaneClient`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FakeRmuxCall {
        Input(RmuxInputAction),
        Resize(TerminalSizeSpec),
        Close,
        Detach,
    }

    /// Deterministic, in-memory [`RmuxPaneClient`] for exercising the
    /// create/attach -> render update -> lag resync -> resize ->
    /// close/detach lifecycle without a real daemon.
    #[derive(Default)]
    pub struct FakeRmuxPaneClient {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        current: PaneSnapshot,
        pending_updates: std::collections::VecDeque<RmuxRenderUpdate>,
        closed: bool,
        calls: Vec<FakeRmuxCall>,
    }

    impl FakeRmuxPaneClient {
        pub fn new(initial_snapshot: PaneSnapshot) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    current: initial_snapshot,
                    ..Default::default()
                }),
            }
        }

        /// Queues a render update that `next_render_update` will yield, in
        /// FIFO order, on subsequent polls.
        pub fn push_render_update(&self, snapshot: PaneSnapshot, lagged: bool) {
            let mut state = self.state.lock().unwrap();
            state.current = snapshot.clone();
            state
                .pending_updates
                .push_back(RmuxRenderUpdate { snapshot, lagged });
        }

        pub fn is_closed(&self) -> bool {
            self.state.lock().unwrap().closed
        }

        pub fn calls(&self) -> Vec<FakeRmuxCall> {
            self.state.lock().unwrap().calls.clone()
        }
    }

    #[async_trait]
    impl RmuxPaneClient for FakeRmuxPaneClient {
        async fn snapshot(&self) -> RmuxResult<PaneSnapshot> {
            Ok(self.state.lock().unwrap().current.clone())
        }

        async fn next_render_update(&self) -> RmuxResult<Option<RmuxRenderUpdate>> {
            if self.is_closed() {
                return Ok(None);
            }
            Ok(self.state.lock().unwrap().pending_updates.pop_front())
        }

        async fn send_input(&self, action: RmuxInputAction) -> RmuxResult<()> {
            self.state
                .lock()
                .unwrap()
                .calls
                .push(FakeRmuxCall::Input(action));
            Ok(())
        }

        async fn resize(&self, size: TerminalSizeSpec) -> RmuxResult<()> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(FakeRmuxCall::Resize(size));
            state.current.cols = size.cols;
            state.current.rows = size.rows;
            Ok(())
        }

        async fn close(&self) -> RmuxResult<()> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(FakeRmuxCall::Close);
            state.closed = true;
            Ok(())
        }

        fn detach(&self) {
            self.state.lock().unwrap().calls.push(FakeRmuxCall::Detach);
        }
    }

    /// Builds a trivially-valid empty snapshot for test setup.
    pub fn empty_snapshot(cols: u16, rows: u16) -> PaneSnapshot {
        let cells = vec![Default::default(); usize::from(cols) * usize::from(rows)];
        PaneSnapshot::new(cols, rows, cells, PaneCursor::default()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use rmux_sdk::TerminalSizeSpec;

    use super::fake::{FakeRmuxCall, FakeRmuxPaneClient, empty_snapshot};
    use super::*;

    #[tokio::test]
    async fn create_and_attach_yields_initial_snapshot() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        let snapshot = client.snapshot().await.unwrap();
        assert_eq!((snapshot.cols, snapshot.rows), (80, 24));
    }

    #[tokio::test]
    async fn render_updates_are_delivered_in_order() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client.push_render_update(empty_snapshot(80, 24), false);
        client.push_render_update(empty_snapshot(80, 24), false);

        assert!(client.next_render_update().await.unwrap().is_some());
        assert!(client.next_render_update().await.unwrap().is_some());
        assert!(client.next_render_update().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn lag_resync_is_surfaced_on_the_update() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client.push_render_update(empty_snapshot(80, 24), true);

        let update = client.next_render_update().await.unwrap().unwrap();
        assert!(update.lagged);
    }

    #[tokio::test]
    async fn resize_updates_snapshot_dimensions_and_is_recorded() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client.resize(TerminalSizeSpec::new(100, 40)).await.unwrap();

        let snapshot = client.snapshot().await.unwrap();
        assert_eq!((snapshot.cols, snapshot.rows), (100, 40));
        assert_eq!(
            client.calls(),
            vec![FakeRmuxCall::Resize(TerminalSizeSpec::new(100, 40))]
        );
    }

    #[tokio::test]
    async fn close_marks_client_closed_and_ends_the_update_stream() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client.push_render_update(empty_snapshot(80, 24), false);

        client.close().await.unwrap();

        assert!(client.is_closed());
        assert_eq!(client.next_render_update().await.unwrap(), None);
        assert_eq!(client.calls(), vec![FakeRmuxCall::Close]);
    }

    #[tokio::test]
    async fn detach_does_not_close_the_pane() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client.detach();

        assert!(!client.is_closed());
        assert_eq!(client.calls(), vec![FakeRmuxCall::Detach]);
    }

    #[tokio::test]
    async fn send_input_records_the_action() {
        let client = FakeRmuxPaneClient::new(empty_snapshot(80, 24));
        client
            .send_input(RmuxInputAction::Text("hi".to_owned()))
            .await
            .unwrap();
        assert_eq!(
            client.calls(),
            vec![FakeRmuxCall::Input(RmuxInputAction::Text("hi".to_owned()))]
        );
    }
}
