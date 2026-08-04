//! RMUX pane-to-pane communication handlers for local-control.
//!
//! NOTE: This implementation supports in-process communication only (same Warp process).
//! Remote SSH/same-host discovery is not supported; SSH RMUX is blocked by missing
//! remote daemon launch capability in rmux-sdk/rmux-server 0.8.0.
//!
//! The request and response shapes here are the ones declared for these actions in
//! `crates/local_control/src/protocol.rs` and advertised by `catalog.rs`. They are built
//! from those types rather than hand-rolled JSON, so a drift between what the catalog
//! advertises and what the bridge accepts becomes a compile error.

use ::local_control::protocol::{
    PaneTarget, RmuxMessageDrainResponse, RmuxMessageSend, RmuxPeerInfo, RmuxPeerListResponse,
    RmuxQueuedMessage, SessionTarget, TargetSelector,
};
use ::local_control::{Action, ActionKind, ControlError, ErrorCode};
use serde::Serialize;
use warpui::AppContext;

use crate::terminal::rmux::service;

/// Resolves the RMUX session name a request is targeting.
fn session_name(target: &TargetSelector, action: ActionKind) -> Result<String, ControlError> {
    target
        .session
        .as_ref()
        .and_then(|session| match session {
            SessionTarget::Id { id } => Some(id.0.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::InvalidSelector,
                format!("{} requires a session target with id", action.as_str()),
            )
        })
}

/// Resolves the RMUX pane id a request is targeting.
fn pane_id(target: &TargetSelector, action: ActionKind) -> Result<u32, ControlError> {
    let raw = target
        .pane
        .as_ref()
        .and_then(|pane| match pane {
            PaneTarget::Id { id } => Some(id.0.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::InvalidSelector,
                format!("{} requires a pane target with id", action.as_str()),
            )
        })?;

    // A pane id that is present but not an RMUX pane number is a malformed
    // selector, not a missing one; saying so beats reporting "requires a pane
    // target" for a target the caller did supply.
    raw.parse::<u32>().map_err(|_| {
        ControlError::with_details(
            ErrorCode::InvalidSelector,
            format!("{} requires a numeric RMUX pane id", action.as_str()),
            format!("received {raw:?}"),
        )
    })
}

fn to_json<T: Serialize>(action: ActionKind, value: &T) -> Result<serde_json::Value, ControlError> {
    serde_json::to_value(value).map_err(|err| {
        ControlError::with_details(
            ErrorCode::Internal,
            format!("failed to serialize {} response", action.as_str()),
            err.to_string(),
        )
    })
}

/// Lists all peers in the given RMUX session.
pub fn rmux_peer_list(
    target: &TargetSelector,
    _ctx: &mut AppContext,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxPeerList;
    let session = session_name(target, action)?;

    let peers = service::list_peers(&session)
        .into_iter()
        .map(|peer| RmuxPeerInfo {
            pane_id: peer.pane_id,
            session_name: peer.session_name,
            is_active: peer.is_active,
        })
        .collect();

    to_json(
        action,
        &RmuxPeerListResponse {
            action: action.as_str(),
            peers,
        },
    )
}

/// Sends a message to a target pane in the same RMUX session.
pub fn rmux_message_send(
    target: &TargetSelector,
    action_request: &Action,
    _ctx: &mut AppContext,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxMessageSend;
    let session = session_name(target, action)?;

    let RmuxMessageSend {
        target_pane_id,
        payload,
    } = serde_json::from_value::<RmuxMessageSend>(action_request.params.clone()).map_err(
        |err| {
            ControlError::with_details(
                ErrorCode::InvalidParams,
                format!(
                    "{} requires target_pane_id and payload params",
                    action.as_str()
                ),
                err.to_string(),
            )
        },
    )?;

    service::send_message(session, target_pane_id, payload)
        .map_err(|err| ControlError::new(ErrorCode::Internal, err))?;

    Ok(crate::local_control::handlers::ack(&None, action))
}

/// Drains pending messages for the target pane in the RMUX session.
pub fn rmux_message_drain(
    target: &TargetSelector,
    _ctx: &mut AppContext,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxMessageDrain;
    let session = session_name(target, action)?;
    let pane = pane_id(target, action)?;

    let messages = service::drain_messages(session, pane)
        .into_iter()
        .map(|message| RmuxQueuedMessage {
            from_pane_id: message.from_pane_id,
            payload: message.payload,
        })
        .collect();

    to_json(
        action,
        &RmuxMessageDrainResponse {
            action: action.as_str(),
            messages,
        },
    )
}
