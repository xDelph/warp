//! RMUX peer discovery and messaging handlers.
//!
//! This module dispatches RMUX local-control actions to the in-process
//! peer queue implementation in `app/src/terminal/rmux/peer.rs`.

use ::local_control::{Action, ActionKind, ControlError};
use warpui::{ModelContext, WindowId};

use crate::local_control::LocalControlBridge;

pub(crate) fn dispatch(
    action: ActionKind,
    action_request: &Action,
    target: &::local_control::protocol::TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    match action {
        ActionKind::RmuxPeerList => {
            crate::terminal::rmux::local_control::rmux_peer_list(target, action_request, ctx)
        }
        ActionKind::RmuxMessageSend => {
            crate::terminal::rmux::local_control::rmux_message_send(target, action_request, ctx)
        }
        ActionKind::RmuxMessageDrain => {
            crate::terminal::rmux::local_control::rmux_message_drain(target, action_request, ctx)
        }
        _ => Err(ControlError::new(
            ::local_control::ErrorCode::NotImplemented,
            format!("RMUX action not implemented: {}", action.as_str()),
        )),
    }
}
