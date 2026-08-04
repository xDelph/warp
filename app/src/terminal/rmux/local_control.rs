//! Local-control handlers for RMUX peer discovery and messaging.
//!
//! This module wires the in-process RMUX peer queue to the local-control
//! action catalog, enabling agents to discover sibling panes in the same
//! RMUX session and queue messages for delivery.

use ::local_control::protocol::{RmuxMessageSend, RmuxPeerInfo, RmuxQueuedMessage};
use ::local_control::{Action, ActionKind, ControlError, ErrorCode};
use serde::Serialize;
use warpui::{ModelContext, ViewHandle, WindowId};

use super::peer::{MessageQueue, PeerInfo, QueuedMessage, RmuxPeer};
use super::terminal_manager::RmuxTerminalManager;
use crate::local_control::LocalControlBridge;
use crate::pane_group::PaneId;
use crate::workspace::Workspace;

/// Global message queue shared by all RMUX panes.
fn message_queue() -> &'static MessageQueue {
    super::terminal_manager::message_queue()
}

/// Extract RMUX session metadata from a terminal pane.
fn extract_rmux_metadata(
    workspace: &Workspace,
    tab_index: usize,
    pane_id: &PaneId,
    ctx: &ModelContext<LocalControlBridge>,
) -> Result<(String, u32), ControlError> {
    workspace.read(ctx, |workspace, _| {
        let pane_group = workspace.get_pane_group_view(tab_index)?;
        let terminal_view = pane_group.get_terminal_view(pane_id)?;
        
        // Access RMUX metadata through the terminal manager
        let terminal_manager = terminal_view
            .terminal_manager()
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::InvalidTarget,
                    "target pane has no terminal manager",
                )
            })?;
        
        let rmux_manager = terminal_manager
            .as_any()
            .downcast_ref::<RmuxTerminalManager>()
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::InvalidTarget,
                    "target pane is not an RMUX session",
                )
            })?;
        
        let session_name = rmux_manager.session_name.clone();
        let pane_id = rmux_manager
            .event_loop
            .as_ref(|event_loop| event_loop.pane_id())
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::InvalidTarget,
                    "RMUX pane ID not yet available",
                )
            })?;
        
        Ok((session_name, pane_id))
    })
}

/// `rmux.peer.list` — list all peer panes in the same RMUX session.
pub(crate) fn rmux_peer_list(
    target: &::local_control::protocol::TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxPeerList;
    let (_window_id, workspace, tab_index, pane_id) =
        crate::local_control::resolver::resolve_target_pane(ctx, target, action)?;

    // Extract RMUX session name from the pane
    let (session_name, _caller_pane_id) = extract_rmux_metadata(workspace, tab_index, pane_id, ctx)?;

    // List peers from the in-process queue
    let peer = RmuxPeer::new(session_name, 0, message_queue().clone());
    let peers = peer.list_peers();

    let peer_infos: Vec<RmuxPeerInfo> = peers
        .into_iter()
        .map(|info| RmuxPeerInfo {
            pane_id: info.pane_id,
            session_name: info.session_name,
            is_active: info.is_active,
        })
        .collect();

    to_json(
        action,
        &::local_control::protocol::RmuxPeerListResponse {
            action: action.as_str(),
            peers: peer_infos,
        },
    )
}

/// `rmux.message.send` — queue a message for delivery to a peer pane.
pub(crate) fn rmux_message_send(
    target: &::local_control::protocol::TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxMessageSend;
    let (_window_id, workspace, tab_index, pane_id) =
        crate::local_control::resolver::resolve_target_pane(ctx, target, action)?;

    // Extract RMUX session name and pane ID from the caller's pane
    let (session_name, caller_pane_id) = extract_rmux_metadata(workspace, tab_index, pane_id, ctx)?;

    // Parse message send parameters
    let params = action_request
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value::<RmuxMessageSend>(p.clone()).ok())
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::InvalidParams,
                format!("{} requires RmuxMessageSend parameters", action.as_str()),
            )
        })?;

    // Queue the message using the in-process peer API
    let peer = RmuxPeer::new(session_name, caller_pane_id, message_queue().clone());
    peer.send_message(params.target_pane_id, params.payload)
        .map_err(|e| {
            ControlError::new(
                ErrorCode::OperationFailed,
                format!("failed to queue message: {}", e),
            )
        })?;

    to_json(
        action,
        &::local_control::protocol::Acknowledgement {
            action: action.as_str(),
            acknowledged: true,
        },
    )
}

/// `rmux.message.drain` — retrieve pending messages for the caller pane.
pub(crate) fn rmux_message_drain(
    target: &::local_control::protocol::TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::RmuxMessageDrain;
    let (_window_id, workspace, tab_index, pane_id) =
        crate::local_control::resolver::resolve_target_pane(ctx, target, action)?;

    // Extract RMUX session name and pane ID from the caller's pane
    let (session_name, caller_pane_id) = extract_rmux_metadata(workspace, tab_index, pane_id, ctx)?;

    // Drain messages using the in-process peer API
    let peer = RmuxPeer::new(session_name, caller_pane_id, message_queue().clone());
    let messages = peer.drain_messages();

    let queued_messages: Vec<RmuxQueuedMessage> = messages
        .into_iter()
        .map(|msg| RmuxQueuedMessage {
            from_pane_id: msg.from_pane_id,
            payload: msg.payload,
        })
        .collect();

    to_json(
        action,
        &::local_control::protocol::RmuxMessageDrainResponse {
            action: action.as_str(),
            messages: queued_messages,
        },
    )
}

/// Helper to serialize a response to JSON.
fn to_json<T: Serialize>(action: ActionKind, response: &T) -> Result<serde_json::Value, ControlError> {
    serde_json::to_value(response).map_err(|e| {
        ControlError::new(
            ErrorCode::InternalError,
            format!("{} response serialization failed: {}", action.as_str(), e),
        )
    })
}
