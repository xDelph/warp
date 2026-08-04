//! In-process pane-to-pane communication for RMUX sessions.
//!
//! Since rmux-sdk v0.8 does not provide inter-pane messaging, this module
//! implements a local in-process queue for panes within the same RMUX session
//! to discover peers and queue messages. Messages are stored in memory and
//! delivered when the target pane polls for them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

/// Largest payload a single queued message may carry.
///
/// Queued messages are held in memory until the target pane drains them, so this
/// bound is what keeps a chatty (or hostile) sender from growing the queue without
/// limit.
pub const MAX_MESSAGE_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Global message queue singleton shared across all RMUX panes.
static GLOBAL_MESSAGE_QUEUE: OnceLock<MessageQueue> = OnceLock::new();

/// Returns the global message queue instance.
pub fn global_message_queue() -> &'static MessageQueue {
    GLOBAL_MESSAGE_QUEUE.get_or_init(MessageQueue::new)
}

/// Unique identifier for a pane within an RMUX session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneKey {
    pub(crate) session_name: String,
    pub(crate) pane_id: u32,
}

impl PaneKey {
    pub(crate) fn new(session_name: String, pane_id: u32) -> Self {
        Self {
            session_name,
            pane_id,
        }
    }
}

/// A message queued for delivery to a pane.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueuedMessage {
    pub from_pane_id: u32,
    pub payload: Vec<u8>,
    pub timestamp_nanos: u128,
}

/// Information about a peer pane in the same session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerInfo {
    pub pane_id: u32,
    pub session_name: String,
    pub is_active: bool,
}

/// In-process message queue for RMUX panes.
///
/// This is a singleton that stores messages keyed by target pane. Since rmux-sdk
/// does not provide inter-pane messaging, we queue messages in-process and
/// deliver them when the target pane polls.
#[derive(Clone)]
pub struct MessageQueue {
    inner: Arc<Mutex<MessageQueueInner>>,
}

struct MessageQueueInner {
    /// Messages keyed by target pane (session_name, pane_id)
    messages: HashMap<PaneKey, Vec<QueuedMessage>>,
    /// Known panes for peer discovery
    known_panes: HashMap<PaneKey, PeerInfo>,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageQueue {
    /// Creates a new message queue.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MessageQueueInner {
                messages: HashMap::new(),
                known_panes: HashMap::new(),
            })),
        }
    }

    /// Registers a pane as discoverable by peers.
    pub fn register_pane(&self, key: PaneKey, info: PeerInfo) {
        let mut inner = self.inner.lock();
        inner.known_panes.insert(key, info);
    }

    /// Unregisters a pane from peer discovery.
    pub fn unregister_pane(&self, key: &PaneKey) {
        let mut inner = self.inner.lock();
        inner.known_panes.remove(key);
        inner.messages.remove(key); // Drop pending messages
    }

    /// Lists all peers in the same session.
    pub fn list_peers(&self, session_name: &str) -> Vec<PeerInfo> {
        let inner = self.inner.lock();
        inner
            .known_panes
            .iter()
            .filter(|(key, _)| key.session_name == session_name)
            .map(|(_, info)| info.clone())
            .collect()
    }

    /// Queues a message for delivery to a target pane.
    ///
    /// Returns an error if the target pane is not registered, or if the payload
    /// exceeds [`MAX_MESSAGE_PAYLOAD_BYTES`].
    pub fn queue_message(
        &self,
        target: PaneKey,
        from_pane_id: u32,
        payload: Vec<u8>,
    ) -> Result<(), QueueError> {
        // Enforced here rather than in a caller because this is the single choke
        // point every sender goes through: the in-process `RmuxPeer` API and the
        // local-control `service::send_message` path both land on it. Messages sit
        // in memory until the target pane polls for them, so an unbounded payload
        // from a local-control client would grow this queue without limit.
        if payload.len() > MAX_MESSAGE_PAYLOAD_BYTES {
            return Err(QueueError::PayloadTooLarge);
        }

        let mut inner = self.inner.lock();
        if !inner.known_panes.contains_key(&target) {
            return Err(QueueError::TargetPaneNotFound);
        }
        inner
            .messages
            .entry(target)
            .or_insert_with(Vec::new)
            .push(QueuedMessage {
                from_pane_id,
                payload,
                timestamp_nanos: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            });
        Ok(())
    }

    /// Retrieves pending messages for a pane.
    ///
    /// Returns all pending messages and clears them from the queue.
    pub fn drain_messages(&self, key: &PaneKey) -> Vec<QueuedMessage> {
        let mut inner = self.inner.lock();
        inner.messages.remove(key).unwrap_or_default()
    }

    /// Returns the number of pending messages for a pane.
    pub fn pending_count(&self, key: &PaneKey) -> usize {
        let inner = self.inner.lock();
        inner.messages.get(key).map(|v| v.len()).unwrap_or(0)
    }
}

/// Errors that can occur when queuing messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// The target pane is not registered in the queue.
    TargetPaneNotFound,
    /// The message payload is too large.
    PayloadTooLarge,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::TargetPaneNotFound => write!(f, "target pane not found"),
            QueueError::PayloadTooLarge => write!(f, "message payload too large"),
        }
    }
}

impl std::error::Error for QueueError {}

/// Peer communication handle for an RMUX pane.
pub struct RmuxPeer {
    key: PaneKey,
}

impl RmuxPeer {
    /// Creates a new peer handle using the global message queue.
    pub fn new(session_name: String, pane_id: u32) -> Self {
        Self {
            key: PaneKey::new(session_name, pane_id),
        }
    }

    /// Registers this pane for peer discovery.
    pub fn register(&self) {
        let info = PeerInfo {
            pane_id: self.key.pane_id,
            session_name: self.key.session_name.clone(),
            is_active: true,
        };
        global_message_queue().register_pane(self.key.clone(), info);
    }

    /// Unregisters this pane from peer discovery.
    pub fn unregister(&self) {
        global_message_queue().unregister_pane(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The message queue is a process-wide singleton and cargo runs these tests in
    // parallel threads inside one process, so a shared session name would let the
    // tests observe each other's peer registrations. Each test gets its own.
    const REGISTER_SESSION: &str = "peer-tests-register";
    const SEND_DRAIN_SESSION: &str = "peer-tests-send-drain";
    const UNREGISTERED_SESSION: &str = "peer-tests-unregistered";
    const TOO_LARGE_SESSION: &str = "peer-tests-too-large";
    const AT_LIMIT_SESSION: &str = "peer-tests-at-limit";
    const SERVICE_LIMIT_SESSION: &str = "peer-tests-service-limit";

    #[test]
    fn test_message_queue_register_unregister() {
        let sender = RmuxPeer::new(REGISTER_SESSION.to_string(), 1);
        let receiver = RmuxPeer::new(REGISTER_SESSION.to_string(), 2);

        sender.register();
        receiver.register();

        assert_eq!(global_message_queue().list_peers(REGISTER_SESSION).len(), 2);

        sender.unregister();
        receiver.unregister();

        assert_eq!(global_message_queue().list_peers(REGISTER_SESSION).len(), 0);
    }

    #[test]
    fn test_queue_send_drain() {
        let sender = RmuxPeer::new(SEND_DRAIN_SESSION.to_string(), 1);
        let receiver = RmuxPeer::new(SEND_DRAIN_SESSION.to_string(), 2);

        sender.register();
        receiver.register();

        let target_key = PaneKey::new(SEND_DRAIN_SESSION.to_string(), 2);
        global_message_queue()
            .queue_message(target_key, 1, b"test message".to_vec())
            .unwrap();

        let receiver_key = PaneKey::new(SEND_DRAIN_SESSION.to_string(), 2);
        assert_eq!(global_message_queue().pending_count(&receiver_key), 1);
        let messages = global_message_queue().drain_messages(&receiver_key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, b"test message");
        assert!(messages[0].timestamp_nanos > 0);
    }

    #[test]
    fn test_queue_send_to_unregistered() {
        let sender = RmuxPeer::new(UNREGISTERED_SESSION.to_string(), 1);

        sender.register();

        let target_key = PaneKey::new(UNREGISTERED_SESSION.to_string(), 999);
        let result = global_message_queue().queue_message(target_key, 1, b"test".to_vec());
        assert_eq!(result, Err(QueueError::TargetPaneNotFound));
    }

    #[test]
    fn test_queue_payload_too_large() {
        let sender = RmuxPeer::new(TOO_LARGE_SESSION.to_string(), 1);
        let receiver = RmuxPeer::new(TOO_LARGE_SESSION.to_string(), 2);

        sender.register();
        receiver.register();

        let target_key = PaneKey::new(TOO_LARGE_SESSION.to_string(), 2);
        let large_payload = vec![0u8; MAX_MESSAGE_PAYLOAD_BYTES + 1];
        let result = global_message_queue().queue_message(target_key, 1, large_payload);
        assert_eq!(result, Err(QueueError::PayloadTooLarge));
    }

    #[test]
    fn test_queue_accepts_payload_at_limit() {
        let sender = RmuxPeer::new(AT_LIMIT_SESSION.to_string(), 1);
        let receiver = RmuxPeer::new(AT_LIMIT_SESSION.to_string(), 2);

        sender.register();
        receiver.register();

        // The bound is inclusive, so exactly the limit must still be accepted;
        // asserting only the over-limit case would pass for an off-by-one that
        // rejects the largest legal message.
        let target_key = PaneKey::new(AT_LIMIT_SESSION.to_string(), 2);
        let payload = vec![0u8; MAX_MESSAGE_PAYLOAD_BYTES];
        assert_eq!(
            global_message_queue().queue_message(target_key, 1, payload),
            Ok(())
        );
    }

    #[test]
    fn test_service_send_enforces_payload_limit() {
        // service::send_message is the local-control entry point and does not go
        // through RmuxPeer, so it needs its own proof that the bound applies.
        let receiver = RmuxPeer::new(SERVICE_LIMIT_SESSION.to_string(), 2);
        receiver.register();

        let too_large = vec![0u8; MAX_MESSAGE_PAYLOAD_BYTES + 1];
        let result = crate::terminal::rmux::service::send_message(
            SERVICE_LIMIT_SESSION.to_string(),
            2,
            too_large,
        );
        assert_eq!(result, Err(QueueError::PayloadTooLarge.to_string()));
    }
}
