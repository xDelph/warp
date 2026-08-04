//! RMUX daemon — long-lived headless process serving local RMUX sessions.
//!
//! Binds rmux-server to a local Unix domain socket and stays alive after
//! GUI exit for true tmux-like persistence. The parent GUI first checks if
//! the socket exists (daemon already running) and only spawns the daemon if
//! absent, then waits until the socket is reachable before connecting.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;

/// Path to the RMUX daemon's Unix domain socket.
pub(super) fn socket_path() -> PathBuf {
    let dir = crate::warp_managed_paths_watcher::warp_data_dir().join("rmux");
    dir.join("rmux.sock")
}

/// Path to the RMUX daemon's PID file (used for flock serialization).
pub(super) fn pid_path() -> PathBuf {
    let dir = crate::warp_managed_paths_watcher::warp_data_dir().join("rmux");
    dir.join("rmux.pid")
}

/// Ensures the RMUX daemon directory exists with owner-only permissions.
fn ensure_daemon_dir() -> anyhow::Result<()> {
    let dir = crate::warp_managed_paths_watcher::warp_data_dir().join("rmux");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
    Ok(())
}

/// Maximum usable `sun_path` length for Unix domain sockets.
const SUN_PATH_MAX: usize = 103;

/// Entry point for `rmux-daemon`.
///
/// Binds rmux-server to the local Unix socket and runs until killed.
/// This is a long-lived process that survives GUI exit for persistence.
pub fn run() -> anyhow::Result<()> {
    let socket_path = socket_path();
    let pid_path = pid_path();

    // Guard against socket paths that exceed the sun_path limit.
    let path_len = socket_path.as_os_str().len();
    if path_len > SUN_PATH_MAX {
        anyhow::bail!(
            "RMUX daemon socket path is {path_len} bytes, which exceeds the \
             sun_path limit of {SUN_PATH_MAX} bytes: {}",
            socket_path.display()
        );
    }

    // Ensure the parent directory exists.
    ensure_daemon_dir()?;

    // Remove any stale socket from a previous crash.
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    // Write PID file for flock serialization.
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    log::info!("RMUX daemon binding to {}", socket_path.display());

    // Create Tokio runtime for the daemon.
    let runtime =
        tokio::runtime::Runtime::new().context("failed to create Tokio runtime for RMUX daemon")?;

    // Bind rmux-server to the Unix socket.
    let config = rmux_server::DaemonConfig::new(socket_path.clone());
    let handle = runtime
        .block_on(rmux_server::ServerDaemon::new(config).bind())
        .context("failed to bind RMUX daemon")?;

    // Set socket permissions to owner-only.
    let _ = std::fs::set_permissions(&socket_path, Permissions::from_mode(0o600));

    log::info!("RMUX daemon bound to {}", socket_path.display());

    // Run the daemon forever (until killed).
    runtime
        .block_on(handle.wait())
        .context("RMUX daemon run loop failed")
}

/// Parent-side helper: check if RMUX daemon is running and start it if not.
///
/// Returns the socket path to connect to. The caller should wait for the
/// socket to become reachable if the daemon was just started.
pub fn ensure_daemon_running() -> anyhow::Result<PathBuf> {
    let socket_path = socket_path();
    let pid_path = pid_path();

    // Ensure the parent directory exists.
    ensure_daemon_dir()?;

    // Acquire exclusive flock on the PID file to serialize concurrent starts.
    let pid_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&pid_path)?;
    let pid_fd = pid_file.as_raw_fd();
    flock_wait(pid_fd, libc::LOCK_EX)?;

    // Check if socket exists and is actually reachable (daemon running).
    if socket_path.exists() {
        // Probe the socket to verify it's actually live.
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            log::info!("RMUX daemon already running, reusing existing socket");
            flock_wait(pid_fd, libc::LOCK_UN)?;
            return Ok(socket_path);
        } else {
            // Socket exists but not reachable — stale from a crash.
            log::info!("RMUX daemon socket is stale, removing and restarting");
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    log::info!("RMUX daemon not running, will start one");

    // Spawn the daemon in a new session so it survives GUI exit.
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("rmux-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid(2) is async-signal-safe and has no side effects
    // other than creating a new session.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("failed to spawn RMUX daemon")?;

    // Wait for the daemon's socket to appear before releasing the flock.
    wait_for_socket(&socket_path)?;

    flock_wait(pid_fd, libc::LOCK_UN)?;
    drop(pid_file);

    log::info!("RMUX daemon started and socket ready");
    Ok(socket_path)
}

/// Wait for a Unix socket to accept connections, with timeout.
fn wait_for_socket(socket_path: &std::path::Path) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(10);
    let start = std::time::Instant::now();

    while std::os::unix::net::UnixStream::connect(socket_path).is_err() {
        if start.elapsed() > timeout {
            anyhow::bail!(
                "timeout waiting for RMUX daemon socket at {}",
                socket_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(())
}

/// Wrapper around libc flock with error handling.
pub(super) fn flock_wait(fd: libc::c_int, operation: libc::c_int) -> anyhow::Result<()> {
    loop {
        let result = unsafe { libc::flock(fd, operation) };
        if result == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err).context("flock failed");
    }
}

/// Testable helper: probe socket liveness by attempting a connection.
///
/// Returns true if the socket exists and accepts connections (daemon running).
/// Returns false if the socket exists but is not connectable (stale) or does not exist.
#[cfg(all(test, unix, feature = "rmux_native_pane"))]
pub(super) fn is_socket_live(socket_path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

#[cfg(all(test, unix, feature = "rmux_native_pane"))]
#[path = "rmux_daemon_tests.rs"]
mod tests;
