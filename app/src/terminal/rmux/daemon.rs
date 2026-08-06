//! RMUX embedded daemon lifecycle for Warp-owned panes.

use std::sync::Arc;

use anyhow::Context as _;
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle, default_socket_path};
use tokio::sync::OnceCell;

/// Global embedded daemon instance.
static EMBEDDED_DAEMON: OnceCell<Arc<ServerHandle>> = OnceCell::const_new();

/// Ensures the embedded RMUX daemon is started and returns its handle.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_embedded_daemon_starts() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            ensure_embedded_daemon()
                .await
                .expect("embedded RMUX starts");
        })
        .await
        .expect("embedded RMUX starts within timeout");
    }
}
