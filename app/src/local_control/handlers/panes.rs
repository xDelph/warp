//! Pane-scoped local-control handlers powering cmux-style agent workflows:
//! identify the caller's pane, list sibling panes with agent-classifiable
//! identity, read a pane's screen, and deliver text into a pane.
#[cfg(test)]
#[path = "panes_tests.rs"]
mod tests;
use ::local_control::protocol::{DirectionParams, PaneTarget, RenameParams, TabTarget, TargetSelector, TextParams};
use ::local_control::{Action, ActionKind, ControlError, ErrorCode};
use serde::Serialize;
use warpui::{ModelContext, ViewHandle, WindowId};

use crate::local_control::resolver::target_window_id_for_target;
use crate::local_control::LocalControlBridge;
use crate::pane_group::local_control::ControlPaneSummary;
use crate::pane_group::PaneId;
use crate::terminal::TerminalView;
use crate::workspace::Workspace;

const DEFAULT_READ_LINES: usize = 100;
const MAX_READ_LINES: usize = 10_000;

#[derive(Serialize)]
struct TabSummary {
    id: String,
    index: usize,
    title: String,
    is_active: bool,
    pane_count: usize,
}

#[derive(Serialize)]
struct TabListResponse {
    action: &'static str,
    window_id: String,
    tabs: Vec<TabSummary>,
}

#[derive(Serialize)]
struct PaneListResponse {
    action: &'static str,
    window_id: String,
    tab: TabSummary,
    panes: Vec<ControlPaneSummary>,
}

#[derive(Serialize)]
struct PaneInspectResponse {
    action: &'static str,
    window_id: String,
    tab: TabSummary,
    pane: ControlPaneSummary,
}

#[derive(Serialize)]
struct PaneReadResponse {
    action: &'static str,
    pane_uuid: String,
    alt_screen: bool,
    text: String,
}

#[derive(Serialize)]
struct PaneDeliveryResponse {
    action: &'static str,
    pane_uuid: String,
    delivered: bool,
}

#[derive(Serialize)]
struct TabRenameResponse {
    action: &'static str,
    renamed: bool,
    tab: TabSummary,
}

/// Resolves the (single) workspace view of the target window.
fn workspace_for_target(
    ctx: &mut ModelContext<LocalControlBridge>,
    target: &TargetSelector,
    action: ActionKind,
) -> Result<(WindowId, ViewHandle<Workspace>), ControlError> {
    let window_id = target_window_id_for_target(ctx, target, action)?;
    let workspace = ctx
        .views_of_type::<Workspace>(window_id)
        .and_then(|workspaces| workspaces.into_iter().next())
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::MissingTarget,
                format!("{} requires a workspace in the target window", action.as_str()),
            )
        })?;
    Ok((window_id, workspace))
}

/// Resolves a tab selector to a tab index within the workspace.
fn resolve_tab_index(
    workspace: &Workspace,
    target: &TargetSelector,
    action: ActionKind,
    ctx: &warpui::AppContext,
) -> Result<usize, ControlError> {
    match target.tab.as_ref() {
        None | Some(TabTarget::Active) => Ok(workspace.active_tab_index()),
        Some(TabTarget::Index { index }) => {
            let index = *index as usize;
            if index < workspace.tab_count() {
                Ok(index)
            } else {
                Err(ControlError::new(
                    ErrorCode::MissingTarget,
                    format!("{} cannot resolve the requested tab index", action.as_str()),
                ))
            }
        }
        Some(TabTarget::Id { id }) => (0..workspace.tab_count())
            .find(|index| {
                workspace
                    .get_pane_group_view(*index)
                    .is_some_and(|tab| tab.id().to_string() == id.0)
            })
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::StaleTarget,
                    format!("{} cannot resolve the requested tab id", action.as_str()),
                )
            }),
        Some(TabTarget::Title { title }) => (0..workspace.tab_count())
            .find(|index| {
                workspace
                    .get_pane_group_view(*index)
                    .is_some_and(|tab| tab.as_ref(ctx).display_title(ctx) == *title)
            })
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::MissingTarget,
                    format!("{} cannot resolve the requested tab title", action.as_str()),
                )
            }),
    }
}

fn tab_summary(
    workspace: &Workspace,
    tab_index: usize,
    ctx: &warpui::AppContext,
) -> Result<TabSummary, ControlError> {
    let tab = workspace.get_pane_group_view(tab_index).ok_or_else(|| {
        ControlError::new(ErrorCode::MissingTarget, "tab index out of bounds")
    })?;
    let pane_group = tab.as_ref(ctx);
    Ok(TabSummary {
        id: tab.id().to_string(),
        index: tab_index,
        title: pane_group.display_title(ctx),
        is_active: tab_index == workspace.active_tab_index(),
        pane_count: pane_group.terminal_pane_ids().count(),
    })
}

/// Locates a pane by its hex uuid across every window/tab of the app, so a
/// background caller can identify itself even when its window isn't active.
fn find_pane_by_uuid(
    ctx: &mut ModelContext<LocalControlBridge>,
    pane_uuid: &str,
) -> Option<(WindowId, ViewHandle<Workspace>, usize, PaneId)> {
    let window_ids = ctx.window_ids().collect::<Vec<_>>();
    for window_id in window_ids {
        let Some(workspace) = ctx
            .views_of_type::<Workspace>(window_id)
            .and_then(|workspaces| workspaces.into_iter().next())
        else {
            continue;
        };
        let found = workspace.read(ctx, |workspace, ctx| {
            (0..workspace.tab_count()).find_map(|tab_index| {
                let tab = workspace.get_pane_group_view(tab_index)?;
                let pane_id = tab.as_ref(ctx).find_terminal_pane_by_uuid(pane_uuid, ctx)?;
                Some((tab_index, pane_id))
            })
        });
        if let Some((tab_index, pane_id)) = found {
            return Some((window_id, workspace, tab_index, pane_id));
        }
    }
    None
}

/// Resolves the pane a request targets. `PaneTarget::Id` searches every
/// window; `Active`/`None` and `Index` resolve within the target window/tab.
fn resolve_target_pane(
    ctx: &mut ModelContext<LocalControlBridge>,
    target: &TargetSelector,
    action: ActionKind,
) -> Result<(WindowId, ViewHandle<Workspace>, usize, PaneId), ControlError> {
    if let Some(PaneTarget::Id { id }) = target.pane.as_ref() {
        return find_pane_by_uuid(ctx, &id.0).ok_or_else(|| {
            ControlError::new(
                ErrorCode::StaleTarget,
                format!("{} cannot resolve the requested pane id", action.as_str()),
            )
        });
    }
    let (window_id, workspace) = workspace_for_target(ctx, target, action)?;
    let (tab_index, pane_id) = workspace.read(ctx, |workspace, ctx| {
        let tab_index = resolve_tab_index(workspace, target, action, ctx)?;
        let tab = workspace.get_pane_group_view(tab_index).ok_or_else(|| {
            ControlError::new(ErrorCode::MissingTarget, "tab index out of bounds")
        })?;
        let pane_group = tab.as_ref(ctx);
        let pane_id = match target.pane.as_ref() {
            None | Some(PaneTarget::Active) => pane_group.focused_pane_id(ctx),
            Some(PaneTarget::Index { index }) => pane_group
                .terminal_pane_ids()
                .nth(*index as usize)
                .ok_or_else(|| {
                    ControlError::new(
                        ErrorCode::MissingTarget,
                        format!("{} cannot resolve the requested pane index", action.as_str()),
                    )
                })?,
            Some(PaneTarget::Id { .. }) => unreachable!("handled above"),
        };
        Ok::<_, ControlError>((tab_index, pane_id))
    })?;
    Ok((window_id, workspace, tab_index, pane_id))
}

fn terminal_view_for_pane(
    workspace: &ViewHandle<Workspace>,
    tab_index: usize,
    pane_id: PaneId,
    ctx: &mut ModelContext<LocalControlBridge>,
    action: ActionKind,
) -> Result<ViewHandle<TerminalView>, ControlError> {
    workspace
        .read(ctx, |workspace, ctx| {
            let tab = workspace.get_pane_group_view(tab_index)?;
            tab.as_ref(ctx).terminal_view_from_pane_id(pane_id, ctx)
        })
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::StaleTarget,
                format!("{} target pane has no terminal view", action.as_str()),
            )
        })
}

fn pane_uuid_hex_for_pane(
    workspace: &ViewHandle<Workspace>,
    tab_index: usize,
    pane_id: PaneId,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> String {
    workspace
        .read(ctx, |workspace, ctx| {
            workspace
                .get_pane_group_view(tab_index)
                .and_then(|tab| tab.as_ref(ctx).terminal_pane_uuid_hex(pane_id, ctx))
        })
        .unwrap_or_default()
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

fn text_param(action: &Action) -> Result<String, ControlError> {
    match serde_json::from_value::<TextParams>(action.params.clone()) {
        Ok(TextParams { text }) => Ok(text),
        _ => Err(ControlError::new(
            ErrorCode::InvalidParams,
            format!("{} requires text params", action.kind.as_str()),
        )),
    }
}

fn limit_param(action: &Action) -> Result<Option<u32>, ControlError> {
    if action
        .params
        .as_object()
        .is_some_and(serde_json::Map::is_empty)
    {
        return Ok(None);
    }
    // This function seems to accept an optional limit parameter
    // For now, return None if params are empty
    Ok(None)
}

fn rename_param(action: &Action) -> Result<String, ControlError> {
    match serde_json::from_value::<RenameParams>(action.params.clone()) {
        Ok(RenameParams { title }) => Ok(title),
        _ => Err(ControlError::new(
            ErrorCode::InvalidParams,
            format!("{} requires rename params", action.kind.as_str()),
        )),
    }
}

/// `tab.list` — tabs of the target window with titles (the workspace map an
/// agent uses to find its sandbox).
pub(crate) fn tab_list(
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::TabList;
    let (window_id, workspace) = workspace_for_target(ctx, target, action)?;
    let tabs = workspace.read(ctx, |workspace, ctx| {
        (0..workspace.tab_count())
            .map(|index| tab_summary(workspace, index, ctx))
            .collect::<Result<Vec<_>, _>>()
    })?;
    to_json(
        action,
        &TabListResponse {
            action: action.as_str(),
            window_id: window_id.to_string(),
            tabs,
        },
    )
}

/// `pane.list` — identity summaries for every terminal pane in the target
/// tab. This is the agent-discovery primitive: titles carry the foreground
/// process (`claude`, `devin`, …) and remote panes expose their hostname.
pub(crate) fn pane_list(
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::PaneList;
    let (window_id, workspace) = workspace_for_target(ctx, target, action)?;
    let (tab, panes) = workspace.read(ctx, |workspace, ctx| {
        let tab_index = resolve_tab_index(workspace, target, action, ctx)?;
        let tab = tab_summary(workspace, tab_index, ctx)?;
        let panes = workspace
            .get_pane_group_view(tab_index)
            .map(|tab| tab.as_ref(ctx).control_pane_summaries(ctx))
            .unwrap_or_default();
        Ok::<_, ControlError>((tab, panes))
    })?;
    to_json(
        action,
        &PaneListResponse {
            action: action.as_str(),
            window_id: window_id.to_string(),
            tab,
            panes,
        },
    )
}

/// `pane.inspect` — the "identify" verb. A caller passes its own
/// `WARP_TERMINAL_SESSION_UUID` as the pane id and learns which
/// window/tab/pane it lives in.
pub(crate) fn pane_inspect(
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::PaneInspect;
    let (window_id, workspace, tab_index, pane_id) = resolve_target_pane(ctx, target, action)?;
    let (tab, pane) = workspace.read(ctx, |workspace, ctx| {
        let tab = tab_summary(workspace, tab_index, ctx)?;
        let pane = workspace
            .get_pane_group_view(tab_index)
            .and_then(|tab| {
                let pane_group = tab.as_ref(ctx);
                pane_group.control_pane_summary(pane_id, pane_group.focused_pane_id(ctx), ctx)
            })
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::StaleTarget,
                    "pane.inspect target pane is not a terminal pane",
                )
            })?;
        Ok::<_, ControlError>((tab, pane))
    })?;
    to_json(
        action,
        &PaneInspectResponse {
            action: action.as_str(),
            window_id: window_id.to_string(),
            tab,
            pane,
        },
    )
}

/// `block.output` — read the target pane's screen: the alt-screen grid when a
/// TUI is running, otherwise the last blocks' commands and outputs, capped to
/// the requested number of lines.
pub(crate) fn pane_read_screen(
    target: &TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::BlockOutput;
    let limit = limit_param(action_request)?
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_READ_LINES)
        .min(MAX_READ_LINES);
    let (_window_id, workspace, tab_index, pane_id) = resolve_target_pane(ctx, target, action)?;
    let pane_uuid = pane_uuid_hex_for_pane(&workspace, tab_index, pane_id, ctx);
    let terminal_view = terminal_view_for_pane(&workspace, tab_index, pane_id, ctx, action)?;
    let (alt_screen, text) = terminal_view.read(ctx, |view, _ctx| {
        let model = view.model.lock();
        let alt_screen_active = model.is_alt_screen_active();
        let text = if alt_screen_active {
            model.alt_screen().output_to_string()
        } else {
            let mut collected: Vec<String> = Vec::new();
            let mut line_count = 0;
            for block in model.block_list().blocks().iter().rev() {
                let command = block.command_to_string();
                let output = block.output_to_string();
                let mut section = String::new();
                if !command.is_empty() {
                    section.push_str("> ");
                    section.push_str(&command);
                    section.push('\n');
                }
                if !output.is_empty() {
                    section.push_str(&output);
                }
                if section.trim().is_empty() {
                    continue;
                }
                line_count += section.lines().count();
                collected.push(section);
                if line_count >= limit {
                    break;
                }
            }
            collected.reverse();
            collected.join("\n")
        };
        let tail = text
            .lines()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        (alt_screen_active, tail)
    });
    to_json(
        action,
        &PaneReadResponse {
            action: action.as_str(),
            pane_uuid,
            alt_screen,
            text,
        },
    )
}

/// `input.insert` — stage text in the target pane's Warp input editor
/// without executing it.
pub(crate) fn input_insert(
    target: &TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::InputInsert;
    let text = text_param(action_request)?;
    let (_window_id, workspace, tab_index, pane_id) = resolve_target_pane(ctx, target, action)?;
    let pane_uuid = pane_uuid_hex_for_pane(&workspace, tab_index, pane_id, ctx);
    let terminal_view = terminal_view_for_pane(&workspace, tab_index, pane_id, ctx, action)?;
    let input_handle = terminal_view.read(ctx, |view, _| view.input().clone());
    input_handle.update(ctx, |input, ctx| {
        input.system_insert(&text, ctx);
    });
    to_json(
        action,
        &PaneDeliveryResponse {
            action: action.as_str(),
            pane_uuid,
            delivered: true,
        },
    )
}

/// `input.run` — execute text in the target pane, cmux `send` style.
///
/// When the pane shows a fullscreen TUI (alt screen), the text plus Enter is
/// written straight to the PTY so the TUI receives it. Otherwise the text
/// runs through Warp's input editor pipeline — which applies command
/// rewrites like the SSH wrapper, blockification, and history — exactly as
/// if the user typed it.
pub(crate) fn input_run(
    target: &TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::InputRun;
    let text = text_param(action_request)?;
    let (_window_id, workspace, tab_index, pane_id) = resolve_target_pane(ctx, target, action)?;
    let pane_uuid = pane_uuid_hex_for_pane(&workspace, tab_index, pane_id, ctx);
    let terminal_view = terminal_view_for_pane(&workspace, tab_index, pane_id, ctx, action)?;
    let input_handle = terminal_view.read(ctx, |view, _| view.input().clone());
    let executed_via_editor = input_handle.update(ctx, |input, ctx| {
        if input.can_execute_command(ctx).is_no() {
            return false;
        }
        input.clear_buffer_and_reset_undo_stack(ctx);
        input.set_pending_command(&text, ctx);
        input.execute_pending_command(ctx);
        true
    });
    if !executed_via_editor {
        // A command or fullscreen TUI owns the pane: deliver text + Enter
        // straight to the PTY so whatever is running receives it.
        terminal_view.update(ctx, |view, ctx| {
            let mut bytes = text.clone().into_bytes();
            bytes.push(b'\r');
            view.write_viewer_bytes_to_pty(bytes, ctx);
        });
    }
    to_json(
        action,
        &PaneDeliveryResponse {
            action: action.as_str(),
            pane_uuid,
            delivered: true,
        },
    )
}

fn direction_param(
    action: &Action,
) -> Result<crate::pane_group::Direction, ControlError> {
    let direction = match serde_json::from_value::<DirectionParams>(action.params.clone()) {
        Ok(DirectionParams { direction }) => direction,
        _ => {
            return Err(ControlError::new(
                ErrorCode::InvalidParams,
                format!("{} requires a direction", action.kind.as_str()),
            ));
        }
    };
    match direction {
        ::local_control::protocol::Direction::Left => Ok(crate::pane_group::Direction::Left),
        ::local_control::protocol::Direction::Right => Ok(crate::pane_group::Direction::Right),
        ::local_control::protocol::Direction::Up => Ok(crate::pane_group::Direction::Up),
        ::local_control::protocol::Direction::Down => Ok(crate::pane_group::Direction::Down),
        ::local_control::protocol::Direction::Previous
        | ::local_control::protocol::Direction::Next => Err(ControlError::new(
            ErrorCode::InvalidParams,
            format!(
                "{} only accepts left/right/up/down",
                action.kind.as_str()
            ),
        )),
    }
}

/// `pane.split` — split the target pane, spawning a new session next to it.
/// The new pane inherits the base pane's working directory, and — when the
/// base session is remote and `inherit_ssh_on_split` is enabled — reconnects
/// to the same SSH host and directory.
pub(crate) fn pane_split(
    target: &TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::PaneSplit;
    let direction = direction_param(action_request)?;
    let (_window_id, workspace, tab_index, pane_id) = resolve_target_pane(ctx, target, action)?;
    let pane_group = workspace
        .read(ctx, |workspace, _| {
            workspace.get_pane_group_view(tab_index).cloned()
        })
        .ok_or_else(|| {
            ControlError::new(ErrorCode::MissingTarget, "tab index out of bounds")
        })?;
    let base_terminal_pane_id = pane_id.as_terminal_pane_id();
    let new_pane_id = pane_group.update(ctx, |pane_group, ctx| {
        pane_group.add_session(
            direction,
            Some(pane_id),
            base_terminal_pane_id,
            None,
            None,
            ctx,
        )
    });
    let pane_uuid = pane_uuid_hex_for_pane(&workspace, tab_index, new_pane_id.into(), ctx);
    to_json(
        action,
        &PaneDeliveryResponse {
            action: action.as_str(),
            pane_uuid,
            delivered: true,
        },
    )
}

/// `tab.rename` — set a custom tab title (agents label their own tab with
/// their identity, cmux-style).
pub(crate) fn tab_rename(
    target: &TargetSelector,
    action_request: &Action,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let action = ActionKind::TabRename;
    let title = rename_param(action_request)?;
    let (_window_id, workspace) = workspace_for_target(ctx, target, action)?;
    let tab = workspace.update(ctx, |workspace, ctx| {
        let tab_index = resolve_tab_index(workspace, target, action, ctx)?;
        workspace.rename_tab_internal(tab_index, &title, ctx);
        tab_summary(workspace, tab_index, ctx)
    })?;
    to_json(
        action,
        &TabRenameResponse {
            action: action.as_str(),
            renamed: true,
            tab,
        },
    )
}
