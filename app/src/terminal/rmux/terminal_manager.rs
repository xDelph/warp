//! [`crate::terminal::TerminalManager`] implementation backed by an RMUX
//! pane, following the same construction shape as
//! [`crate::terminal::remote_tty::TerminalManager`]: build the
//! `TerminalModel`/`TerminalView` pair synchronously, then connect to the
//! backing session (here, the RMUX daemon) from a background task.

use std::any::Any;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use async_channel::Sender;
use parking_lot::FairMutex;
use pathfinder_geometry::vector::Vector2F;
use rmux_sdk::TerminalSizeSpec;
use warpui::{AppContext, ModelHandle, ViewHandle, WindowId};

use super::client::RmuxPaneClient;
use super::event_loop::{EventLoop, RmuxEventLoopMessage};
use super::input::map_pty_bytes_to_rmux_input;
use super::peer::{MessageQueue, RmuxPeer};
use super::types::{RmuxPaneOwnership, RmuxPaneSpec};
use crate::terminal::pane_allocator;
use crate::context_chips::prompt_type::PromptType;
use crate::pane_group::pane::DetachType;
use crate::pane_group::TerminalViewResources;
use crate::persistence::ModelEvent;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::session::Sessions;
use crate::terminal::model_events::ModelEventDispatcher;
use crate::terminal::shell::{ShellName, ShellType};
use crate::terminal::{
    self as terminal_module, terminal_manager, ShellLaunchState, TerminalModel, TerminalView,
};
use crate::terminal::terminal_manager::BlockSpacing;

/// [`crate::terminal::TerminalManager`] for a single RMUX-backed pane.
pub struct RmuxTerminalManager {
    model: Arc<FairMutex<TerminalModel>>,
    view: ViewHandle<TerminalView>,
    event_loop: ModelHandle<EventLoop>,
    message_tx: Sender<RmuxEventLoopMessage>,
    pub(super) session_name: String,
    cwd: Option<String>,
    ownership: RmuxPaneOwnership,
    peer: Option<RmuxPeer>,
    message_queue: MessageQueue,
}

impl RmuxTerminalManager {
    /// Creates an RMUX-backed terminal manager, view, and model, and starts
    /// connecting to the RMUX daemon in the background.
    pub fn create_model(
        spec: RmuxPaneSpec,
        resources: TerminalViewResources,
        initial_size: Vector2F,
        window_id: WindowId,
        model_event_sender: Option<SyncSender<ModelEvent>>,
        ctx: &mut AppContext,
    ) -> (ViewHandle<TerminalView>, ModelHandle<Box<dyn terminal_module::TerminalManager>>) {
        let ownership = spec.ownership;
        let session_name = spec.session_name.clone();
        let cwd = spec.cwd.clone();
        let message_queue = MessageQueue::new();

        let (wakeups_tx, wakeups_rx) = async_channel::unbounded();
        let (events_tx, events_rx) = async_channel::unbounded();
        let (executor_command_tx, _executor_command_rx) = async_channel::unbounded();
        // RMUX panes don't broadcast PTY bytes to other consumers (recordings,
        // etc); use a capacity-1 channel since `async_broadcast` requires at
        // least 1.
        let (pty_reads_tx, _pty_reads_rx) = async_broadcast::broadcast(1);

        let channel_event_proxy = ChannelEventListener::new(wakeups_tx, events_tx, pty_reads_tx);

        let sessions: ModelHandle<Sessions> =
            ctx.add_model(|ctx| Sessions::new(executor_command_tx, ctx));
        let model_events =
            ctx.add_model(|ctx| ModelEventDispatcher::new(events_rx, sessions.clone(), ctx));

        let startup_directory = spec.cwd.as_ref().map(PathBuf::from);
        let model = terminal_manager::create_terminal_model(
            startup_directory,
            None, /* restored_blocks */
            initial_size,
            channel_event_proxy.clone(),
            // RMUX owns the actual shell process; this placeholder mirrors
            // remote_tty's network-backed PTY, which has the same property.
            ShellLaunchState::ShellSpawned {
                available_shell: None,
                display_name: ShellName::blank(),
                shell_type: ShellType::Zsh,
            },
            BlockSpacing::for_gui(ctx),
            ctx,
        );

        let size_info = *model.block_list().size();
        let colors = model.colors();
        let model = Arc::new(FairMutex::new(model));

        let prompt_type =
            ctx.add_model(|ctx| PromptType::new_dynamic_from_sessions(sessions.clone(), ctx));

        let cloned_model = model.clone();
        let view = ctx.add_typed_action_view(window_id, |ctx| {
            TerminalView::new(
                resources,
                wakeups_rx,
                model_events.clone(),
                cloned_model,
                sessions.clone(),
                size_info,
                colors,
                model_event_sender.clone(),
                prompt_type,
                None, // initial_input_config
                None, // conversation_restoration
                None, // inactive_pty_reads_rx
                false, // is_cloud_mode
                ctx,
            )
        });

        let (message_tx, message_rx) = async_channel::unbounded();

        let weak_view = view.downgrade();
        let event_loop = ctx.add_model(|ctx| {
            EventLoop::start(
                model.clone(),
                spec,
                message_rx,
                channel_event_proxy,
                weak_view,
                ctx,
            )
        });

        Self::wire_up_view_events(&view, message_tx.clone(), ctx);

        let manager = Self {
            model,
            view: view.clone(),
            event_loop,
            message_tx,
            session_name,
            cwd,
            ownership,
            peer: None,
            message_queue,
        };

        let terminal_manager = ctx.add_model(|_ctx| {
            let manager: Box<dyn terminal_module::TerminalManager> = Box::new(manager);
            manager
        });

        (view, terminal_manager)
    }

    /// Builds a persistence snapshot for this pane.
    pub fn snapshot(&self, uuid: Vec<u8>, ctx: &AppContext) -> crate::app_state::RmuxTerminalPaneSnapshot {
        let pane_id = self.event_loop.as_ref(ctx).pane_id();
        crate::app_state::RmuxTerminalPaneSnapshot {
            uuid,
            session_name: self.session_name.clone(),
            pane_id,
            cwd: self.cwd.clone(),
            warp_created: self.ownership.is_warp_created(),
        }
    }

    fn wire_up_view_events(
        view: &ViewHandle<TerminalView>,
        message_tx: Sender<RmuxEventLoopMessage>,
        ctx: &mut AppContext,
    ) {
        ctx.subscribe_to_view(view, move |_view, event, _ctx| {
            #[allow(clippy::single_match)]
            match event {
                terminal_module::Event::WriteBytesToPty { bytes } => {
                    let action = map_pty_bytes_to_rmux_input(bytes);
                    if let Err(error) =
                        message_tx.try_send(RmuxEventLoopMessage::Input(action))
                    {
                        log::warn!("failed to queue RMUX pane input: {error}");
                    }
                }
                terminal_module::Event::Resize { size_update } => {
                    let new_size = size_update.new_size;
                    let size = TerminalSizeSpec::new(
                        new_size.columns().min(u16::MAX as usize) as u16,
                        new_size.rows().min(u16::MAX as usize) as u16,
                    );
                    if let Err(error) =
                        message_tx.try_send(RmuxEventLoopMessage::Resize(size))
                    {
                        log::warn!("failed to queue RMUX pane resize: {error}");
                    }
                }
                _ => {}
            }
        });
    }

    fn client(&self, ctx: &AppContext) -> Option<Arc<dyn RmuxPaneClient>> {
        self.event_loop.as_ref(ctx).client()
    }

    /// Ensures peer is registered if not already.
    /// Called lazily when peer functionality is needed.
    fn ensure_peer_registered(&mut self, ctx: &AppContext) {
        if self.peer.is_some() {
            return;
        }

        if let Some(pane_id) = self.event_loop.as_ref(ctx).pane_id() {
            let peer = RmuxPeer::new(
                self.session_name.clone(),
                pane_id,
                self.message_queue.clone(),
            );
            peer.register();
            self.peer = Some(peer);
            log::info!("Registered RMUX peer: session={}, pane_id={}", self.session_name, pane_id);
        }
    }

    /// Returns the peer handle for this pane, registering if needed.
    pub fn peer(&mut self, ctx: &AppContext) -> Option<&RmuxPeer> {
        self.ensure_peer_registered(ctx);
        self.peer.as_ref()
    }
}

impl terminal_module::TerminalManager for RmuxTerminalManager {
    fn model(&self) -> Arc<FairMutex<TerminalModel>> {
        self.model.clone()
    }

    fn on_view_detached(&self, detach_type: DetachType, app: &mut AppContext) {
        if !matches!(detach_type, DetachType::Closed) {
            return;
        }

        let _ = self.message_tx.try_send(RmuxEventLoopMessage::Shutdown);

        // Unregister peer if registered
        if let Some(peer) = &self.peer {
            peer.unregister();
        }

        // Release pane name from allocator if this was a Warp-created pane
        if matches!(self.ownership, RmuxPaneOwnership::WarpCreated) {
            pane_allocator::global_allocator().release(&self.session_name);
        }

        let Some(client) = self.client(app) else {
            return;
        };

        match self.ownership {
            RmuxPaneOwnership::WarpCreated => {
                app.background_executor()
                    .spawn(async move {
                        if let Err(error) = client.close().await {
                            log::error!("failed to close RMUX pane on Warp pane close: {error}");
                        }
                    })
                    .detach();
            }
            RmuxPaneOwnership::ExternallyAttached => client.detach(),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
