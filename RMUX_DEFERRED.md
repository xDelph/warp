# RMUX Integration - Deferred Features

This document tracks features that were intentionally deferred from the RMUX
integration but are designed to be added incrementally in future work.

## Verification Record (2026-08-04)

### Verified

- `CARGO_TARGET_DIR=/tmp/warp-rmux-final cargo check -p warp --features
  rmux_native_pane --lib` completed with zero errors.
- `./script/format` completed.
- `CARGO_TARGET_DIR=/tmp/warp-rmux-final cargo clippy -p warp --features
  rmux_native_pane --lib -- -D warnings` completed without reported diagnostics.
- The persistent-daemon worker, local-control handlers, RMUX-by-default
  routing, pane naming, and app-state persistence paths compile together.

### Not Verified

- Launching the GUI and proving that the detached `rmux-daemon` worker survives
  a Warp restart, retains a live shell, and reattaches the saved pane id.
- Recovery from an actual daemon crash with a stale Unix socket. The focused
  test build was stopped before its test binary linked and ran.
- SSH RMUX panes, remote daemon/socket forwarding, and cross-process or
  cross-host peer messaging. These remain deferred.
- Full cmux feature parity, including workspace/layout operations beyond Warp's
  existing pane and workspace features.

## V1 Scope (Complete)

The V1 implementation includes:
- Embedded daemon lifecycle management
- Client wrapper with daemon handle and pane_id capture
- Event loop with pane_id tracking and event emission
- Terminal manager with lazy peer registration
- Pane allocator for stable naming
- RMUX-by-default for new local panes
- Manual RMUX pane creation via command palette
- Snapshot structure and creation (in-session restoration)
- Restoration logic (tab/window moves within live session)
- Cleanup on detach

## V2 Scope (Complete)

Three features listed as deferred in V1 have since been implemented. They are
recorded here so this document does not contradict the code:

- **EventLoop event subscription.** `RmuxTerminalManager::create_model` now
  subscribes to the event loop and registers the pane as a peer on
  `EventLoopEvent::Connected { pane_id: Some(..) }`, replacing lazy registration.
  See `app/src/terminal/rmux/terminal_manager.rs`.
- **Peer communication usage.** The peer module is reachable from local-control
  through `rmux.peer.list`, `rmux.message.send`, and `rmux.message.drain`
  (`app/src/local_control/handlers/rmux.rs`, dispatched in `bridge.rs`). The
  message queue is a process-wide singleton (`peer::global_message_queue`) so
  panes created by different terminal managers can address each other; a
  per-manager queue could never have delivered anything between panes.
- **Stable session naming.** `pane_group` derives a base command from the chosen
  shell and passes it to `allocate_rmux_session_name`, yielding `codex-1`,
  `claude-1`, `gemini-1`, and `shell-N` for ordinary shells.

### Sender identity for local-control messages

`rmux.message.send` attributes messages to `from_pane_id: 0`, a reserved
service-level identity, rather than impersonating the calling pane. Local-control
clients are not themselves RMUX panes, so there is no caller pane id to use, and
fabricating one would make a message appear to come from a real peer.

## Deferred Features

### 1. External Pane Attachment

**Status:** Ownership variant exists but never used

**Details:**
- `RmuxPaneOwnership::ExternallyAttached` enum variant exists
- Only `WarpCreated` panes are ever created in the codebase
- No UI or code path for attaching to external RMUX sessions
- Cleanup logic handles both ownership types, but only one is used

**Why Deferred:**
- Current scope is Warp-owned panes only
- External attachment would require:
  - UI for selecting/connecting to external RMUX sessions
  - Session discovery mechanism (list available sessions)
  - Authentication/authorization for external sessions
  - Different lifecycle semantics (detach vs close)

**Future Implementation:**
```rust
// Example: Attach to external RMUX session
let spec = RmuxPaneSpec::new(
    "external-session-name",
    RmuxPaneOwnership::ExternallyAttached,
);
let (view, manager) = RmuxTerminalManager::create_model(spec, ...);
```

### 2. SSH / Remote Panes

**Status:** Design complete, implementation deferred

**Details:**
- SSH sessions fall back to `LocalTty`; see the fallback branch in
  `app/src/pane_group/mod.rs`
- Warp has existing remote-server infrastructure that can be extended for SSH RMUX

**Architecture:**
Warp's remote-server infrastructure provides:
- `remote-server-proxy` - thin SSH process that bridges stdio to daemon Unix socket
- `remote-server-daemon` - long-lived daemon on remote host
- SSH transport (`SshTransport`) - uses ControlMaster sockets for multiplexing
- Binary installer - deploys remote-server binary to remote host

**SSH RMUX Extension Design:**
1. Add `rmux-proxy` subcommand (analogous to `remote-server-proxy`)
   - Launched over SSH via existing transport
   - Checks if RMUX daemon is running (PID file + `kill -0`)
   - Starts RMUX daemon if not running
   - Bridges SSH stdio to RMUX daemon Unix socket

2. Add `rmux-daemon` subcommand (analogous to `remote-server-daemon`)
   - Long-lived process on remote host
   - Uses rmux-sdk to manage RMUX sessions
   - Binds Unix domain socket for proxy connections
   - Exits after grace period with no connections

3. Integration points:
   - Extend `SshTransport` with RMUX mode flag
   - Add RMUX socket/PID path helpers (similar to remote-server)
   - Use existing binary installer to deploy RMUX-enabled binary
   - Reuse proxy pattern from `app/src/remote_server/unix/proxy.rs`

4. Socket paths:
   - RMUX daemon socket: `~/.warp[-channel]/rmux/rmux.sock`
   - RMUX daemon PID: `~/.warp[-channel]/rmux/rmux.pid`

**Implementation approach:**
- Add `rmux_proxy()` and `rmux_daemon()` entry points in `app/src/remote_server/mod.rs`
- Implement RMUX proxy in `app/src/remote_server/unix/rmux_proxy.rs` (similar to proxy.rs)
- Implement RMUX daemon in `app/src/remote_server/unix/rmux_daemon.rs` (similar to mod.rs daemon)
- Add RMUX mode to `SshTransport::remote_proxy_command()`
- Update SSH split logic to use RMUX mode when SSH RMUX is enabled

**Why Deferred:**
- Design is complete and feasible using existing infrastructure
- Implementation requires significant new code (proxy, daemon, transport integration)
- Prioritized local RMUX and peer communication first

### 3. Cross-Process Peer Communication

**Status:** In-process only

**Details:**
- `peer::global_message_queue()` is a `OnceLock` singleton, so it is shared
  across every pane inside **one** Warp process
- Two Warp processes, or a Warp process and an external RMUX client, cannot
  exchange messages
- Delivery is poll-based: messages sit in the queue until the target drains them

**Why Deferred:**
- Cross-process delivery needs a real transport (daemon-mediated or socket) plus
  a permission model for who may address whom

## Persistence

RMUX panes are persisted to the app-state database
(`LeafContents::RmuxTerminal(_) => true` in `app/src/app_state.rs`). Warp-owned
panes use a detached `rmux-daemon` worker so the daemon is intended to outlive
the GUI process and retain sessions across restart.

This restart/reattach behavior is not yet runtime-verified. See the verification
record above for the exact remaining proof obligations.

## Rollout

`RmuxNativePane` is in `RELEASE_FLAGS` and `rmux_native_pane` is in the default
Cargo feature set, so RMUX panes are on for all users, not just dogfood builds.
Both switches live in `crates/warp_features/src/lib.rs` and `app/Cargo.toml`.

## Implementation Notes

### Testing

- `app/src/terminal/rmux/peer.rs` has unit tests for registration, send/drain,
  unknown targets, and the payload size limit. Because the queue is a
  process-wide singleton and cargo runs tests in parallel threads within one
  process, **each test must use a unique session name** or the tests race each
  other's peer registrations.
- EventLoop event emission is tested indirectly via connection tests
- Pane allocator has tests for stable naming patterns

### Compilation Status

```bash
cargo check -p warp --features rmux_native_pane
cargo test -p warp --features rmux_native_pane --lib terminal::rmux::peer
```

Note the package is `warp`; `cargo check -p app` does not resolve.

## Related Documentation

- `SPEC.md` - Complete RMUX and ACP integration specification
- `AGENTS.md` - Warp development guidelines
- `app/src/terminal/rmux/` - RMUX implementation source code
