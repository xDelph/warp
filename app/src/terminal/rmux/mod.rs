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
mod event_loop;
mod grid;
mod input;
mod terminal_manager;
mod types;

pub use terminal_manager::RmuxTerminalManager;
pub use types::{RmuxPaneOwnership, RmuxPaneSpec};
