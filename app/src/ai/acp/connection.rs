//! Warp's ACP connection wrapper.
//!
//! Based on the official agent-client-protocol SDK but auto-approves
//! `session/request_permission` so local agents can run tools without blocking
//! on a client that doesn't implement the UI yet.

use std::cell::RefCell;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use async_fs;
use async_process::{Child, Command, Stdio};
use futures::channel::{mpsc, oneshot};

#[derive(Clone, Debug, Default)]
struct SessionUpdateBroadcaster {
    subscribers: Rc<RefCell<Vec<mpsc::UnboundedSender<acp::SessionNotification>>>>,
}

impl SessionUpdateBroadcaster {
    fn subscribe(&self) -> mpsc::UnboundedReceiver<acp::SessionNotification> {
        let (tx, rx) = mpsc::unbounded();
        self.subscribers.borrow_mut().push(tx);
        rx
    }

    fn publish(&self, notification: &acp::SessionNotification) {
        let mut subscribers = self.subscribers.borrow_mut();
        subscribers.retain(|subscriber| subscriber.unbounded_send(notification.clone()).is_ok());
    }
}

#[derive(Clone, Debug)]
struct ConnectionClient {
    session_updates: SessionUpdateBroadcaster,
    cwd: PathBuf,
}

impl ConnectionClient {
    fn new(session_updates: SessionUpdateBroadcaster, cwd: PathBuf) -> Self {
        Self {
            session_updates,
            cwd,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for ConnectionClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let Some(option) = preferred_permission_option(&args.options) else {
            return Err(acp::Error::internal_error());
        };

        Ok(acp::RequestPermissionResponse::new(
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                option.option_id.clone(),
            )),
        ))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        self.session_updates.publish(&args);
        Ok(())
    }

    async fn read_text_file(
        &self,
        args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        let path = resolve_path_within_cwd(&args.path, &self.cwd)
            .ok_or_else(acp::Error::invalid_params)?;
        let content = async_fs::read_to_string(path)
            .await
            .map_err(|_| acp::Error::internal_error())?;
        Ok(acp::ReadTextFileResponse::new(read_line_range(
            content, args.line, args.limit,
        )))
    }

    async fn write_text_file(
        &self,
        args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        let path = resolve_path_within_cwd(&args.path, &self.cwd)
            .ok_or_else(acp::Error::invalid_params)?;
        if let Some(parent) = path.parent() {
            async_fs::create_dir_all(parent)
                .await
                .map_err(|_| acp::Error::internal_error())?;
        }
        async_fs::write(path, args.content)
            .await
            .map_err(|_| acp::Error::internal_error())?;
        Ok(acp::WriteTextFileResponse::new())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Err(acp::Error::method_not_found())
    }
}

fn preferred_permission_option(
    options: &[acp::PermissionOption],
) -> Option<&acp::PermissionOption> {
    options
        .iter()
        .find(|option| option.kind == acp::PermissionOptionKind::AllowAlways)
        .or_else(|| {
            options
                .iter()
                .find(|option| option.kind == acp::PermissionOptionKind::AllowOnce)
        })
        .or_else(|| options.first())
}

struct ConnectionState {
    connection: Option<Rc<acp::ClientSideConnection>>,
    child: Option<Child>,
    io_task: Option<oneshot::Receiver<Result<()>>>,
}

/// A connected ACP client bound to a local subprocess.
pub(crate) struct Connection {
    session_updates: SessionUpdateBroadcaster,
    state: Rc<RefCell<ConnectionState>>,
}

impl Connection {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self> {
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|source| anyhow::anyhow!("Failed to spawn process: {source}"))?;
        let outgoing = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Missing child stdin"))?;
        let incoming = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Missing child stdout"))?;
        let session_updates = SessionUpdateBroadcaster::default();
        let cwd = command
            .get_current_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let client = ConnectionClient::new(session_updates.clone(), cwd);

        let (connection, io_task) =
            acp::ClientSideConnection::new(client, outgoing, incoming, |task| {
                tokio::task::spawn_local(task);
            });
        let connection = Rc::new(connection);
        let (io_task_tx, io_task_rx) = oneshot::channel();

        tokio::spawn(async move {
            let _ = io_task_tx.send(
                io_task
                    .await
                    .map_err(|e| anyhow::anyhow!("IO task error: {e}")),
            );
        });

        Ok(Self {
            session_updates,
            state: Rc::new(RefCell::new(ConnectionState {
                connection: Some(connection),
                child: Some(child),
                io_task: Some(io_task_rx),
            })),
        })
    }

    pub(crate) fn subscribe_session_updates(
        &self,
    ) -> mpsc::UnboundedReceiver<acp::SessionNotification> {
        self.session_updates.subscribe()
    }

    pub(crate) async fn close(&self) -> Result<()> {
        let (connection, mut child, io_task) = {
            let mut state = self.state.borrow_mut();
            let Some(connection) = state.connection.take() else {
                return Ok(());
            };

            (connection, state.child.take(), state.io_task.take())
        };

        drop(connection);

        if let Some(child) = child.as_mut() {
            match child.kill() {
                Ok(()) => {}
                Err(source) if source.kind() == ErrorKind::InvalidInput => {}
                Err(source) => return Err(anyhow!("Failed to kill process: {source}")),
            }
        }

        if let Some(mut child) = child {
            child
                .status()
                .await
                .map_err(|source| anyhow!("Failed to wait for process: {source}"))?;
        }

        if let Some(io_task) = io_task {
            // Never block shutdown on stdio EOF: agents launched through a
            // shim chain (e.g. codex-acp's bun symlink → `npm exec` → node →
            // platform binary) leave grandchildren holding the pipes after
            // the direct child dies, so the IO task may never see EOF.
            // Dropping the receiver detaches it; the reader is torn down with
            // its runtime.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), io_task).await;
        }

        Ok(())
    }

    pub(crate) async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse> {
        self.connection()?
            .initialize(args)
            .await
            .map_err(|e| anyhow!("Initialize failed: {e}"))
    }

    pub(crate) async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse> {
        self.connection()?
            .authenticate(args)
            .await
            .map_err(|e| anyhow!("Authenticate failed: {e}"))
    }

    pub(crate) async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse> {
        self.connection()?
            .new_session(args)
            .await
            .map_err(|e| anyhow!("New session failed: {e}"))
    }

    pub(crate) async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse> {
        self.connection()?
            .set_session_mode(args)
            .await
            .map_err(|e| anyhow!("Set session mode failed: {e}"))
    }

    pub(crate) async fn prompt(&self, args: acp::PromptRequest) -> Result<acp::PromptResponse> {
        self.connection()?
            .prompt(args)
            .await
            .map_err(|e| anyhow!("Prompt failed: {e}"))
    }

    pub(crate) async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse> {
        self.connection()?
            .set_session_config_option(args)
            .await
            .map_err(|e| anyhow!("Set session config option failed: {e}"))
    }

    pub(crate) async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> Result<acp::SetSessionModelResponse> {
        self.connection()?
            .set_session_model(args)
            .await
            .map_err(|e| anyhow!("Set session model failed: {e}"))
    }

    pub(crate) async fn ext_method(&self, args: acp::ExtRequest) -> Result<acp::ExtResponse> {
        self.connection()?
            .ext_method(args)
            .await
            .map_err(|e| anyhow!("Ext method failed: {e}"))
    }

    fn connection(&self) -> Result<Rc<acp::ClientSideConnection>> {
        self.state
            .borrow()
            .connection
            .clone()
            .ok_or_else(|| anyhow!("Connection closed"))
    }
}

pub(crate) fn initialize_request() -> acp::InitializeRequest {
    acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(
            acp::ClientCapabilities::new().fs(acp::FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true)),
        )
        .client_info(acp::Implementation::new("warp", env!("CARGO_PKG_VERSION")).title("Warp"))
}

fn resolve_path_within_cwd(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().ok()?;
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let normalized_path = normalize_path(&path)?;

    if normalized_path.exists() {
        return normalized_path
            .canonicalize()
            .ok()
            .filter(|path| path.starts_with(&cwd));
    }

    let mut ancestor = normalized_path.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent()?;
    }
    ancestor
        .canonicalize()
        .ok()
        .filter(|path| path.starts_with(&cwd))
        .map(|_| normalized_path)
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    Some(normalized)
}

fn read_line_range(content: String, line: Option<u32>, limit: Option<u32>) -> String {
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let limit = limit.map(|limit| limit as usize);
    let lines = content.lines().skip(start);
    match limit {
        Some(limit) => lines.take(limit).collect::<Vec<_>>().join("\n"),
        None => lines.collect::<Vec<_>>().join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{initialize_request, read_line_range, resolve_path_within_cwd};

    #[test]
    fn read_line_range_uses_one_based_lines_and_limit() {
        let content = "one\ntwo\nthree\nfour\n".to_string();
        assert_eq!(read_line_range(content, Some(2), Some(2)), "two\nthree");
    }

    #[test]
    fn initialize_request_advertises_concrete_fs_capabilities() {
        let value = serde_json::to_value(initialize_request()).unwrap();
        assert_eq!(
            value["clientCapabilities"]["fs"]["readTextFile"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            value["clientCapabilities"]["fs"]["writeTextFile"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn resolve_path_within_cwd_accepts_nested_new_file() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir(&cwd).unwrap();
        let path = cwd.join("src").join("main.rs");

        assert_eq!(resolve_path_within_cwd(&path, &cwd), Some(path));
    }

    #[test]
    fn resolve_path_within_cwd_rejects_parent_traversal_for_new_file() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir(&cwd).unwrap();
        let path = cwd.join("..").join("outside.txt");

        assert_eq!(resolve_path_within_cwd(&path, &cwd), None);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_within_cwd_rejects_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join("link")).unwrap();

        let path = PathBuf::from("link").join("secret.txt");

        assert_eq!(resolve_path_within_cwd(&path, &cwd), None);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let (connection, child, io_task) = {
            let mut state = self.state.borrow_mut();
            (
                state.connection.take(),
                state.child.take(),
                state.io_task.take(),
            )
        };

        drop(connection);
        drop(io_task);
        drop(child);
    }
}
