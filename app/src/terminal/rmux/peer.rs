//! In-process pane-to-pane communication for RMUX sessions.
//!
//! Since rmux-sdk v0.8 does not provide inter-pane messaging, this module
//! implements a local in-process queue for panes within the same RMUX session
//! to discover peers and queue messages. Messages are stored in memory and
//! delivered when the target pane polls for them.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// Unique identifier for a pane within an RMUX session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PaneKey {
    session_name: String,
    pane_id: u32,
}

impl PaneKey {
    fn new(session_name: String, pane_id: u32) -> Self {
        Self { session_name, pane_id }
    }
}

/// A message queued for delivery to a pane.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub from_pane_id: u32,
    pub payload: Vec<u8>,
    pub timestamp: std::time::Instant,
}

/// Information about a peer pane in the same session.
#[derive(Debug, Clone)]
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
    /// Returns an error if the target pane is not registered.
    pub fn queue_message(
        &self,
        target: PaneKey,
        from_pane_id: u32,
        payload: Vec<u8>,
    ) -> Result<(), QueueError> {
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
                timestamp: std::time::Instant::now(),
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
    queue: MessageQueue,
}

impl RmuxPeer {
    /// Creates a new peer handle.
    pub fn new(session_name: String, pane_id: u32, queue: MessageQueue) -> Self {
        Self {
            key: PaneKey::new(session_name, pane_id),
            queue,
        }
    }

    /// Registers this pane for peer discovery.
    pub fn register(&self) {
        let info = PeerInfo {
            pane_id: self.key.pane_id,
            session_name: self.key.session_name.clone(),
            is_active: true,
        };
        self.queue.register_pane(self.key, info);
    }

    /// Unregisters this pane from peer discovery.
    pub fn unregister(&self) {
        self.queue.unregister_pane(&self.key);
    }

    /// Lists all peers in the same session.
    pub fn list_peers(&self) -> Vec<PeerInfo> {
        self.queue.list_peers(&self.key.session_name)
    }

    /// Queues a message for delivery to another pane in the same session.
    pub fn send_message(&self, target_pane_id: u32, payload: Vec<u8>) -> Result<(), QueueError> {
        const MAX_PAYLOAD_SIZE: usize = 1024 * 1024; // 1MB limit
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(QueueError::PayloadTooLarge);
        }

        let target = PaneKey::new(self.key.session_name.clone(), target_pane_id);
        self.queue.queue_message(target, self.key.pane_id, payload)
    }

    /// Retrieves pending messages for this pane.
    pub fn drain_messages(&self) -> Vec<QueuedMessage> {
        self.queue.drain_messages(&self.key)
    }

    /// Returns the number of pending messages for this pane.
    pub fn pending_count(&self) -> usize {
        self.queue.pending_count(&self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_queue_register_unregister() {
        let queue = MessageQueue::new();
        let sender = RmuxPeer::new("session".to_string(), 1, queue.clone());
        let receiver = RmuxPeer::new("session".to_string(), 2, queue.clone());

        sender.register();
        receiver.register();

        assert_eq!(queue.list_peers("session").len(), 2);

        sender.unregister();
        receiver.unregister();

        assert_eq!(queue.list_peers("session").len(), 0);
    }

    #[test]
    fn test_peer_send_drain() {
        let queue = MessageQueue::new();
        let sender = RmuxPeer::new("session".to_string(), 1, queue.clone());
        let receiver = RmuxPeer::new("session".to_string(), 2, queue.clone());

        sender.register();
        receiver.register();

        sender.send_message(2, b"test message".to_vec()).unwrap();

        assert_eq!(receiver.pending_count(), 1);
        let messages = receiver.drain_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, b"test message");
    }

    #[test]
    fn test_peer_send_to_unregistered() {
        let queue = MessageQueue::new();
        let sender = RmuxPeer::new("session".to_string(), 1, queue.clone());

        sender.register();

        let result = sender.send_message(999, b"test".to_vec());
        assert_eq!(result, Err(QueueError::TargetPaneNotFound));
    }

    #[test]
    fn test_peer_payload_too_large() {
        let queue = MessageQueue::new();
        let sender = RmuxPeer::new("session".to_string(), 1, queue.clone());
        let receiver = RmuxPeer::new("session".to_string(), 2, queue.clone());

        sender.register();
        receiver.register();

        let large_payload = vec![0u8; 1024 * 1024 + 1]; // 1MB + 1 byte
        let result = sender.send_message(2, large_payload);
        assert_eq!(result, Err(QueueError::PayloadTooLarge));
    }
}
