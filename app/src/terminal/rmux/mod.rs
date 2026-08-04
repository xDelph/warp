//! RMUX-backed native terminal pane (feature-gated by
//! [`crate::features::FeatureFlag::RmuxNativePane`]).
//!
//! V1 supports a single RMUX pane per Warp pane: create, attach, render,
//! resize, input, focus, and close. See `app/src/terminal/rmux/*` for the
//! pipeline: [`grid`] turns an `rmux_sdk::PaneSnapshot` into a synthesized
//! ANSI byte sequence, [`input`] turns Warp keyboard input into RMUX
//! `send_text`/`send_key` calls, [`client`] abstracts the live daemon
//! connection (with a fake for tests), and [`event_loop`] /
//! [`terminal_manager`] wire those pieces into the existing
//! [`crate::terminal::TerminalManager`] / `TerminalView` pane UX.

mod client;
mod daemon;
mod event_loop;
mod grid;
mod input;
mod peer;
mod terminal_manager;
mod types;

pub use daemon::ensure_embedded_daemon;
pub use event_loop::EventLoopEvent;
pub use peer::{PeerInfo, QueueError, QueuedMessage, global_message_queue};
pub use terminal_manager::RmuxTerminalManager;
pub use types::{RmuxPaneOwnership, RmuxPaneSpec};

/// Public service API for RMUX pane-to-pane communication.
pub mod service {
    use super::peer::{PeerInfo, QueuedMessage, global_message_queue};

    /// Lists all peers in the given RMUX session.
    pub fn list_peers(session_name: &str) -> Vec<PeerInfo> {
        global_message_queue().list_peers(session_name)
    }

    /// Sends a message to a target pane in the same RMUX session.
    ///
    /// Uses a service-level sender identity (pane_id: 0) rather than faking the caller's identity.
    pub fn send_message(
        session_name: String,
        target_pane_id: u32,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        let sender_pane_id = 0u32;
        let target_key = super::peer::PaneKey::new(session_name, target_pane_id);
        global_message_queue()
            .queue_message(target_key, sender_pane_id, payload)
            .map_err(|e| e.to_string())
    }

    /// Drains pending messages for the target pane in the RMUX session.
    pub fn drain_messages(session_name: String, pane_id: u32) -> Vec<QueuedMessage> {
        let key = super::peer::PaneKey::new(session_name, pane_id);
        global_message_queue().drain_messages(&key)
    }
}
