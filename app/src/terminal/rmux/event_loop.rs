//! Async event loop bridging an RMUX pane to a Warp [`TerminalModel`].
//!
//! Mirrors the shape of [`crate::terminal::remote_tty::event_loop::EventLoop`]:
//! a `ModelHandle`-owned actor that drives background I/O via
//! `ctx.background_executor()` and `ctx.spawn`, rather than a raw OS thread,
//! since there is no local fd to poll.

use std::sync::Arc;

use async_channel::Receiver;
use parking_lot::FairMutex;
use rmux_sdk::TerminalSizeSpec;
use warpui::{Entity, ModelContext, WeakViewHandle};

use super::client::{connect_rmux_pane, RmuxPaneClient};
use super::grid::render_pane_snapshot_as_ansi;
use super::input::RmuxInputAction;
use super::types::RmuxPaneSpec;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::{TerminalModel, TerminalView};
use crate::view_components::ToastFlavor;

/// Outgoing message forwarded from the view to the RMUX pane.
pub(super) enum RmuxEventLoopMessage {
    Input(RmuxInputAction),
    Resize(TerminalSizeSpec),
    Shutdown,
}

pub(super) struct EventLoop {
    terminal_model: Arc<FairMutex<TerminalModel>>,
    channel_event_listener: ChannelEventListener,
    message_rx: Receiver<RmuxEventLoopMessage>,
    weak_view: WeakViewHandle<TerminalView>,
    client: Option<Arc<dyn RmuxPaneClient>>,
}

impl EventLoop {
    /// Starts the event loop by connecting to the RMUX daemon in the
    /// background and, once connected, driving the render-update and
    /// outgoing-message loops.
    pub(super) fn start(
        model: Arc<FairMutex<TerminalModel>>,
        pane_spec: RmuxPaneSpec,
        message_rx: Receiver<RmuxEventLoopMessage>,
        channel_event_listener: ChannelEventListener,
        weak_view: WeakViewHandle<TerminalView>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let event_loop = Self {
            terminal_model: model,
            channel_event_listener,
            message_rx,
            weak_view,
            client: None,
        };

        ctx.spawn(connect_rmux_pane(pane_spec), Self::on_connected);

        event_loop
    }

    fn on_connected(
        &mut self,
        result: anyhow::Result<Arc<dyn RmuxPaneClient>>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok(client) => self.attach_client(client, ctx),
            Err(error) => {
                log::error!("failed to start RMUX pane: {error:#}");
                if let Some(view) = self.weak_view.upgrade(ctx) {
                    view.update(ctx, |view, ctx| {
                        view.show_persistent_toast(
                            format!("Couldn't start the RMUX pane: {error}"),
                            ToastFlavor::Error,
                            ctx,
                        );
                    });
                }
            }
        }
    }

    fn attach_client(&mut self, client: Arc<dyn RmuxPaneClient>, ctx: &mut ModelContext<Self>) {
        self.client = Some(client.clone());
        self.spawn_render_loop(client.clone(), ctx);
        self.spawn_outgoing_loop(client, ctx);
    }

    fn spawn_render_loop(&self, client: Arc<dyn RmuxPaneClient>, ctx: &mut ModelContext<Self>) {
        let model = self.terminal_model.clone();
        let listener = self.channel_event_listener.clone();
        ctx.background_executor()
            .spawn(async move {
                match client.snapshot().await {
                    Ok(snapshot) => {
                        apply_snapshot(&model, &snapshot);
                        listener.send_wakeup_event();
                    }
                    Err(error) => {
                        log::error!("failed to capture initial RMUX pane snapshot: {error}");
                        return;
                    }
                }

                loop {
                    match client.next_render_update().await {
                        Ok(Some(update)) => {
                            if update.lagged {
                                log::warn!(
                                    "RMUX pane render stream lagged; forcing a full resync"
                                );
                            }
                            apply_snapshot(&model, &update.snapshot);
                            listener.send_wakeup_event();
                        }
                        Ok(None) => break,
                        Err(error) => {
                            log::error!("RMUX pane render stream error: {error}");
                            break;
                        }
                    }
                }
            })
            .detach();
    }

    fn spawn_outgoing_loop(&self, client: Arc<dyn RmuxPaneClient>, ctx: &mut ModelContext<Self>) {
        let message_rx = self.message_rx.clone();
        ctx.background_executor()
            .spawn(async move {
                while let Ok(message) = message_rx.recv().await {
                    match message {
                        RmuxEventLoopMessage::Input(action) => {
                            if let Err(error) = client.send_input(action).await {
                                log::error!("failed to forward input to RMUX pane: {error}");
                            }
                        }
                        RmuxEventLoopMessage::Resize(size) => {
                            if let Err(error) = client.resize(size).await {
                                log::error!("failed to resize RMUX pane: {error}");
                            }
                        }
                        RmuxEventLoopMessage::Shutdown => break,
                    }
                }
            })
            .detach();
    }

    pub(super) fn client(&self) -> Option<Arc<dyn RmuxPaneClient>> {
        self.client.clone()
    }
}

fn apply_snapshot(model: &Arc<FairMutex<TerminalModel>>, snapshot: &rmux_sdk::PaneSnapshot) {
    let bytes = render_pane_snapshot_as_ansi(snapshot);
    model.lock().apply_external_pane_snapshot(&bytes);
}

impl Entity for EventLoop {
    type Event = ();
}
