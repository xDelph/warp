use super::*;

#[test]
fn test_socket_path_format() {
    let socket_path = socket_path();
    assert!(socket_path.ends_with("rmux.sock"));
}

#[test]
fn test_pid_path_format() {
    let pid_path = pid_path();
    assert!(pid_path.ends_with("rmux.pid"));
}

#[test]
fn test_socket_path_under_data_dir() {
    let socket_path = socket_path();
    let data_dir = crate::warp_managed_paths_watcher::warp_data_dir();
    assert!(socket_path.starts_with(data_dir));
}

#[test]
fn test_stale_socket_not_live() {
    let socket_path = socket_path();
    let pid_path = pid_path();

    // Ensure the directory exists.
    ensure_daemon_dir().unwrap();

    // Create a stale socket file (not actually bound, just a file).
    std::fs::write(&socket_path, "stale").unwrap();

    // Verify the socket exists.
    assert!(socket_path.exists());

    // Verify the liveness probe detects it as not live.
    assert!(!is_socket_live(&socket_path), "stale socket should not be live");

    // Clean up.
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);
}
