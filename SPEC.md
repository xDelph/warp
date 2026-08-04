# Warp Terminal - ACP + RMUX Integration Specification

## Overview

This specification documents the custom ACP (Agent Client Protocol) and RMUX (Rust-based terminal multiplexer) integration built on top of the original Warp terminal product. These integrations enable local agent execution with automatic permission approval and native terminal pane multiplexing with embedded daemon support.

---

## ACP Integration

### Purpose

Integrate Warp with the Agent Client Protocol (ACP) to enable local agent execution (e.g., codex-acp, claude-cli) with automatic permission approval, eliminating the need for interactive permission prompts during agent tool execution.

### Architecture

#### Connection Wrapper (`app/src/ai/acp/connection.rs`)

**Core Component:** `Connection` struct wrapping `acp::ClientSideConnection`

**Key Features:**
- Auto-approves `session/request_permission` by selecting the first available permission option
- Prefers `AllowAlways` → `AllowOnce` → first option in order
- Binds to local subprocess via stdio pipes
- Provides session update broadcasting to multiple subscribers
- Implements file system capabilities (read/write text files)

**API Surface:**
```rust
pub struct Connection {
    session_updates: SessionUpdateBroadcaster,
    state: Rc<RefCell<ConnectionState>>,
}

impl Connection {
    pub async fn spawn(command: String, args: Vec<String>) -> Result<Self>
    pub async fn initialize(&self, args: acp::InitializeRequest) -> Result<acp::InitializeResponse>
    pub async fn authenticate(&self, args: acp::AuthenticateRequest) -> Result<acp::AuthenticateResponse>
    pub async fn new_session(&self, args: acp::NewSessionRequest) -> Result<acp::NewSessionResponse>
    pub async fn prompt(&self, args: acp::PromptRequest) -> Result<acp::PromptResponse>
    pub async fn set_session_mode(&self, args: acp::SetSessionModeRequest) -> Result<acp::SetSessionModeResponse>
    pub async fn set_session_config_option(&self, args: acp::SetSessionConfigOptionRequest) -> Result<acp::SetSessionConfigOptionResponse>
    pub async fn set_session_model(&self, args: acp::SetSessionModelRequest) -> Result<acp::SetSessionModelResponse>
    pub async fn ext_method(&self, args: acp::ExtRequest) -> Result<acp::ExtResponse>
    pub async fn close(&self) -> Result<()>
    pub fn subscribe_session_updates(&self) -> mpsc::UnboundedReceiver<acp::SessionNotification>
}
```

**Permission Auto-Approval Logic:**
```rust
async fn request_permission(&self, args: acp::RequestPermissionRequest) -> acp::Result<acp::RequestPermissionResponse> {
    let Some(option) = preferred_permission_option(&args.options) else {
        return Err(acp::Error::internal_error());
    };
    Ok(acp::RequestPermissionResponse::new(
        acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
            option.option_id.clone(),
        ))
    ))
}

fn preferred_permission_option(options: &[acp::PermissionOption]) -> Option<&acp::PermissionOption> {
    options
        .iter()
        .find(|option| option.kind == acp::PermissionOptionKind::AllowAlways)
        .or_else(|| options.iter().find(|option| option.kind == acp::PermissionOptionKind::AllowOnce))
        .or_else(|| options.first())
}
```

**File System Capabilities:**
- `read_text_file`: Reads file content with optional line range support
- `write_text_file`: Writes file content with automatic parent directory creation
- Path resolution constrained to current working directory for security

**Process Management:**
- Spawns agent subprocess with configurable command and arguments
- Manages stdio pipes for ACP communication
- Graceful shutdown with 2-second timeout on IO task EOF
- Handles shim chain processes (e.g., bun symlink → npm exec → node)

#### Transcript Formatting (`app/src/ai/acp/transcript.rs`)

**Purpose:** Convert Warp's ACP output to acpx-compatible structured transcript format

**Data Types:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcpxTranscriptEntry {
    #[serde(rename = "acpx.session")]
    Session { agent: String, mode: String, permission_mode: String, acp_session_id: String, runtime_session_name: String },
    #[serde(rename = "acpx.status")]
    Status { tag: String, values: serde_json::Value },
    #[serde(rename = "acpx.text_delta")]
    TextDelta { text: String, channel: String, tag: Option<String> },
    #[serde(rename = "acpx.tool_call")]
    ToolCall { name: String, tool_call_id: String, status: String, text: Option<String>, input: Option<serde_json::Value> },
    #[serde(rename = "acpx.done")]
    Done { error: Option<String> },
}
```

**Conversion Logic:**
- Extracts plain text from `AIAgentText` sections
- Maps Warp's `LocalAcpStreamChunk` to acpx entries:
  - `Text` → `TextDelta` with channel "output"
  - `Thought` → `TextDelta` with channel "thought"
  - `ToolCall` → `ToolCall` with running status
- Generates session metadata with agent display name and runtime session name format: `warp-acpx-{agent_name_lowercase}`

**Display Formatting:**
- Human-readable output with bracketed prefixes: `[session]`, `[status]`, `[thinking]`, `[tool]`, `[done]`
- Tool status indicators: running, completed, failed

### Integration Points

**Feature Flag:** `local_acp` (compile-time)

**Usage Context:**
- Local agent execution via CLI agent harness integration
- Agent team mode for multi-agent coordination
- Child agent spawning for subtasks

---

## RMUX Integration

### Purpose

Integrate RMUX (Rust-based terminal multiplexer) as the native backend for Warp terminal panes, enabling:
- Embedded daemon lifecycle management (no external `rmux` binary required)
- Pane-to-pane communication within sessions
- Stable pane naming and persistence
- Self-contained RMUX integration within Warp process

### Architecture

#### Module Structure (`app/src/terminal/rmux/`)

```
rmux/
├── daemon.rs          # Embedded RMUX daemon lifecycle
├── client.rs          # RMUX SDK client wrapper
├── event_loop.rs      # Async event loop for pane updates
├── peer.rs            # In-process pane-to-pane communication
├── terminal_manager.rs # Terminal manager for RMUX panes
├── types.rs           # RMUX pane types and specs
└── mod.rs             # Module exports
```

#### Embedded Daemon (`daemon.rs`)

**Purpose:** Manage embedded RMUX server lifecycle within Warp process

**Core Components:**
```rust
static EMBEDDED_DAEMON: OnceCell<Arc<ServerHandle>> = OnceCell::const_new();

pub async fn ensure_embedded_daemon() -> anyhow::Result<Arc<ServerHandle>> {
    EMBEDDED_DAEMON
        .get_or_try_init(|| async {
            let socket_path = default_socket_path().context("resolve RMUX socket path")?;
            let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
                .bind()
                .await
                .context("bind embedded RMUX daemon")?;
            Ok::<_, anyhow::Error>(Arc::new(handle))
        })
        .await
        .cloned()
}
```

**API Used (from rmux-server crate):**
- `rmux_server::default_socket_path()` - Resolve default socket path
- `rmux_server::DaemonConfig::new(socket_path)` - Create daemon config
- `rmux_server::ServerDaemon::new(config).bind().await` - Start daemon
- `rmux_server::ServerHandle` - Daemon handle for lifecycle management

**Lifecycle:**
- Daemon started on first Warp-owned pane creation
- Global singleton ensures single daemon instance
- Daemon lives as long as Warp-owned panes exist (via RAII handle in client)

#### Client Wrapper (`client.rs`)

**Purpose:** Wrap RMUX SDK pane client with daemon handle and pane_id capture

**Core Types:**
```rust
pub struct RmuxSdkPaneClient {
    pane: rmux_sdk::Pane,
    render_stream: tokio::sync::Mutex<Option<rmux_sdk::PaneRenderStream>>,
    _daemon_handle: Option<Arc<ServerHandle>>, // Keeps daemon alive
}

impl RmuxSdkPaneClient {
    pub fn new(pane: rmux_sdk::Pane) -> Self
    pub fn with_daemon_handle(pane: rmux_sdk::Pane, daemon_handle: Arc<ServerHandle>) -> Self
}
```

**Connection Function:**
```rust
pub async fn connect_rmux_pane(
    spec: RmuxPaneSpec,
) -> anyhow::Result<(Arc<dyn RmuxPaneClient>, Option<u32>)> {
    // 1. Ensure embedded daemon for Warp-owned panes
    let daemon_handle = if matches!(spec.ownership, RmuxPaneOwnership::WarpCreated) {
        Some(ensure_embedded_daemon().await?)
    } else { None };

    // 2. Connect to RMUX daemon
    let rmux = rmux_sdk::Rmux::builder().connect_or_start().await?;

    // 3. Ensure session exists
    let session = rmux.ensure_session(ensure).await?;

    // 4. Get pane (by_id for restoration, or default pane(0,0))
    let pane = match spec.pane_id {
        Some(pane_id) => session.pane_by_id(rmux_sdk::PaneId::from(pane_id)).await?,
        None => session.pane(0, 0),
    };

    // 5. Capture pane_id for persistence
    let captured_pane_id = if spec.pane_id.is_none() {
        pane.id().await.context("read RMUX pane id")?.map(Into::into)
    } else { spec.pane_id };

    // 6. Create client with daemon handle
    let client = match daemon_handle {
        Some(handle) => RmuxSdkPaneClient::with_daemon_handle(pane, handle),
        None => RmuxSdkPaneClient::new(pane),
    };

    Ok((Arc::new(client), captured_pane_id))
}
```

**Key Features:**
- Returns tuple: (client, captured_pane_id)
- Supports pane restoration via `pane_by_id()`
- Captures pane_id from SDK for new panes
- Daemon handle stored in client for RAII lifecycle

#### Event Loop (`event_loop.rs`)

**Purpose:** Async event loop bridging RMUX pane to Warp TerminalModel

**Core Structure:**
```rust
pub struct EventLoop {
    terminal_model: Arc<FairMutex<TerminalModel>>,
    channel_event_listener: ChannelEventListener,
    message_rx: Receiver<RmuxEventLoopMessage>,
    weak_view: WeakViewHandle<TerminalView>,
    client: Option<Arc<dyn RmuxPaneClient>>,
    pane_id: Option<u32>,
}

pub enum RmuxEventLoopMessage {
    Input(RmuxInputAction),
    Resize(TerminalSizeSpec),
    Shutdown,
}
```

**Lifecycle:**
1. Spawn `connect_rmux_pane` task in background
2. On connection: attach client, spawn render loop, spawn outgoing loop
3. Render loop: polls for snapshots, applies to TerminalModel
4. Outgoing loop: forwards input/resize messages to RMUX pane
5. On shutdown: close pane (WarpCreated) or detach (ExternallyAttached)

**Pane ID Tracking:**
- Stores pane_id from connection result
- Provides accessor for TerminalManager snapshot
- Note: Peer registration deferred due to view→manager wiring complexity

#### Peer Communication (`peer.rs`)

**Purpose:** In-process pane-to-pane communication for RMUX sessions (since rmux-sdk v0.8 lacks inter-pane messaging)

**Core Types:**
```rust
pub struct MessageQueue {
    inner: Arc<Mutex<MessageQueueInner>>,
}

struct MessageQueueInner {
    messages: HashMap<PaneKey, Vec<QueuedMessage>>,
    known_panes: HashMap<PaneKey, PeerInfo>,
}

pub struct RmuxPeer {
    key: PaneKey,
    queue: MessageQueue,
}

pub struct QueuedMessage {
    pub from_pane_id: u32,
    pub payload: Vec<u8>,
    pub timestamp: std::time::Instant,
}

pub struct PeerInfo {
    pub pane_id: u32,
    pub session_name: String,
    pub is_active: bool,
}
```

**API Surface:**
```rust
impl MessageQueue {
    pub fn new() -> Self
    pub fn register_pane(&self, key: PaneKey, info: PeerInfo)
    pub fn unregister_pane(&self, key: &PaneKey)
    pub fn list_peers(&self, session_name: &str) -> Vec<PeerInfo>
    pub fn queue_message(&self, target: PaneKey, from_pane_id: u32, payload: Vec<u8>) -> Result<(), QueueError>
    pub fn drain_messages(&self, key: &PaneKey) -> Vec<QueuedMessage>
    pub fn pending_count(&self, key: &PaneKey) -> usize
}

impl RmuxPeer {
    pub fn new(session_name: String, pane_id: u32, queue: MessageQueue) -> Self
    pub fn register(&self)
    pub fn unregister(&self)
    pub fn list_peers(&self) -> Vec<PeerInfo>
    pub fn send_message(&self, target_pane_id: u32, payload: Vec<u8>) -> Result<(), QueueError>
    pub fn drain_messages(&self) -> Vec<QueuedMessage>
    pub fn pending_count(&self) -> usize
}
```

**Constraints:**
- 1MB payload size limit
- Target pane must be registered before queuing
- Messages dropped on pane unregistration

#### Terminal Manager (`terminal_manager.rs`)

**Purpose:** TerminalManager implementation for RMUX-backed panes

**Core Structure:**
```rust
pub struct RmuxTerminalManager {
    model: Arc<FairMutex<TerminalModel>>,
    view: ViewHandle<TerminalView>,
    event_loop: ModelHandle<EventLoop>,
    message_tx: Sender<RmuxEventLoopMessage>,
    pub(super) session_name: String,
    cwd: Option<String>,
    ownership: RmuxPaneOwnership,
}
```

**Creation:**
```rust
pub fn create_model(
    spec: RmuxPaneSpec,
    resources: TerminalViewResources,
    initial_size: Vector2F,
    window_id: WindowId,
    model_event_sender: Option<SyncSender<ModelEvent>>,
    ctx: &mut AppContext,
) -> (ViewHandle<TerminalView>, ModelHandle<Box<dyn TerminalManager>>)
```

**Snapshot:**
```rust
pub fn snapshot(&self, uuid: Vec<u8>) -> RmuxTerminalPaneSnapshot {
    RmuxTerminalPaneSnapshot {
        uuid,
        session_name: self.session_name.clone(),
        pane_id: None, // Deferred to event_loop, currently disabled
        cwd: self.cwd.clone(),
        warp_created: self.ownership.is_warp_created(),
    }
}
```

**Lifecycle:**
- On `DetachType::Closed`: send shutdown, close pane (WarpCreated) or detach (ExternallyAttached)
- Release pane name from allocator on close for WarpCreated panes

#### Types (`types.rs`)

**Core Types:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmuxPaneOwnership {
    WarpCreated,      // Warp owns lifecycle, closes pane on detach
    ExternallyAttached, // Warp only detaches, never closes
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmuxPaneSpec {
    pub session_name: String,
    pub cwd: Option<String>,
    pub ownership: RmuxPaneOwnership,
    pub pane_id: Option<u32>, // For restoration
}

impl RmuxPaneSpec {
    pub fn new(session_name: impl Into<String>, ownership: RmuxPaneOwnership) -> Self
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self
    pub fn with_pane_id(mut self, pane_id: u32) -> Self
    pub fn from_snapshot(snapshot: &RmuxTerminalPaneSnapshot) -> Self
}
```

#### Pane Allocator (`terminal/pane_allocator.rs`)

**Purpose:** Global allocator for stable pane naming (e.g., codex-1, codex-2)

**Core Structure:**
```rust
pub struct PaneNameAllocator {
    inner: Arc<Mutex<HashMap<String, AllocationState>>>,
}

struct AllocationState {
    allocated: HashSet<u32>,
    next_suffix: u32,
}
```

**API Surface:**
```rust
impl PaneNameAllocator {
    pub fn new() -> Self
    pub fn allocate(&self, base_command: &str) -> String
    pub fn mark_allocated(&self, name: &str) // For restoration
    pub fn release(&self, name: &str) // On true close
    pub fn list_allocated(&self, base_command: &str) -> Vec<String>
    pub fn count_allocated(&self, base_command: &str) -> usize
    pub fn allocate_rmux_session_name(&self, base_command: Option<&str>) -> String
}

pub fn global_allocator() -> &'static PaneNameAllocator
```

**Allocation Logic:**
- Finds lowest unallocated suffix (1, 2, 3...)
- Reuses lowest available suffix after release
- Preserves suffix while detached (not released)
- Updates next_suffix to avoid conflicts
- Falls back to UUID-based naming if no base command provided

### Integration Points

#### Feature Flag: `rmux_native_pane` (compile-time)

#### Pane Creation (`pane_group/mod.rs`)

**RMUX-by-Default for New Local Panes:**
```rust
#[cfg(all(feature = "local_tty", feature = "rmux_native_pane"))]
let is_new_local_pane = matches!(is_shared_session, IsSharedSessionCreator::No)
    && conversation_restoration.is_none()
    && initial_input_config.is_none();

if is_new_local_pane {
    use crate::terminal::rmux::{RmuxPaneOwnership, RmuxPaneSpec};
    use crate::terminal::pane_allocator;

    let session_name = pane_allocator::global_allocator()
        .allocate_rmux_session_name(None);
    let spec = RmuxPaneSpec::new(session_name, RmuxPaneOwnership::WarpCreated)
        .with_cwd(cwd.to_string_lossy());

    let (terminal_view, terminal_manager) = RmuxTerminalManager::create_model(spec, ...);
} else {
    // Fall back to LocalTty for SSH, shared sessions, restorations
}
```

**Fallback Conditions:**
- SSH sessions (remote_tty)
- Shared sessions (IsSharedSessionCreator::Yes)
- Conversation restoration (conversation_restoration != None)
- Initial input config set (initial_input_config != None)

#### Persistence

**Snapshot Structure (`app_state.rs`):**
```rust
pub struct RmuxTerminalPaneSnapshot {
    pub uuid: Vec<u8>,
    pub session_name: String,
    pub pane_id: Option<u32>, // Currently None (disabled)
    pub cwd: Option<String>,
    pub warp_created: bool,
}
```

**Restoration:**
- `RmuxPaneSpec::from_snapshot()` reconstructs spec from snapshot
- `pane_id` passed through to `connect_rmux_pane()`
- Restoration uses `session.pane_by_id()` if pane_id present
- Legacy snapshots with `pane_id=None` fall back to `session.pane(0, 0)`

**Current State:**
- pane_id capture implemented but disabled in snapshot (pane_id: None)
- Pane ID stored in event_loop but not exposed to snapshot
- Peer registration deferred due to wiring complexity

### Dependencies

**Cargo.toml (app/Cargo.toml):**
```toml
[dependencies]
rmux-server = { version = "0.8.0", optional = true }
rmux-sdk = { version = "0.8.0", optional = true }
which = { version = "4.0", optional = true }

[features]
rmux_native_pane = ["dep:rmux-server", "dep:rmux-sdk", "dep:which"]
local_acp = ["dep:agent_client_protocol"]
```

**ACP Dependency:**
```toml
agent_client_protocol = "..." # ACP SDK
```

---

## Implementation Status

### ACP Integration
- ✅ Connection wrapper with auto-approval
- ✅ File system capabilities (read/write)
- ✅ Session update broadcasting
- ✅ Transcript formatting (acpx-compatible)
- ✅ Process management with graceful shutdown

### RMUX Integration
- ✅ Embedded daemon lifecycle
- ✅ Client wrapper with daemon handle
- ✅ Event loop for render updates
- ✅ Pane ID tracking and persistence
- ✅ Terminal manager implementation
- ✅ Pane allocator for stable naming
- ✅ RMUX-by-default for new local panes
- ✅ Fallback to LocalTty for SSH/shared/restoration
- ✅ Pane ID persistence (enabled in snapshot)
- ✅ Peer communication module (complete)
- ✅ Lazy peer registration (on first access)
- ⚠️ Peer functionality not yet used in application code

---

## Known Limitations

### ACP
- Auto-approval bypasses security review for local agents
- No UI for permission review (hardcoded to approve)
- File operations constrained to CWD (may be too restrictive)

### RMUX
- pane_id persistence disabled (currently always None in snapshot)
- Peer registration not wired into terminal manager
- No external pane attachment (only WarpCreated panes)
- RMUX SDK private API usage (rmux-server crate, no semver guarantees)
- Session naming uses UUID when base command unknown (no stable naming for non-agent panes)

---

## Future Work

### ACP
- [ ] Add permission review UI for local agents
- [ ] Support custom permission policies
- [ ] Expand file system capabilities (binary files, directories)
- [ ] Add tool execution capabilities

### RMUX
- [ ] Enable pane_id persistence in snapshots
- [ ] Wire peer registration into terminal manager
- [ ] Support external pane attachment (ExternallyAttached)
- [ ] Stable session naming for agent panes (integrate with agent context)
- [ ] Add pane restoration by_id tests
- [ ] Migrate to public RMUX SDK API when available
