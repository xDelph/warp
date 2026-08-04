use std::collections::HashMap;
use std::time::{Duration, Instant};

use warp_cli::agent::Harness;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::conversation::AIConversationId;

/// Default TTL for session owner (matches acpx default of 300s)
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
struct SessionInfo {
    session_id: String,
    last_used: Instant,
}

pub(crate) struct LocalAcpSessionStore {
    sessions_by_harness: HashMap<Harness, SessionInfo>,
    /// The harness that most recently completed a prompt for each
    /// conversation. Used to detect harness switches mid-conversation so the
    /// new harness's session can be primed with the prior transcript. Only
    /// recorded on successful completion, which matches the ACP worker session
    /// lifetime (failed sessions are torn down and restarted context-free).
    last_harness_by_conversation: HashMap<AIConversationId, Harness>,
    session_ttl: Duration,
}

impl LocalAcpSessionStore {
    pub(crate) fn new(_: &mut ModelContext<Self>) -> Self {
        Self {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: DEFAULT_SESSION_TTL,
        }
    }

    pub(crate) fn session_id(&self, harness: Harness) -> Option<&str> {
        self.sessions_by_harness
            .get(&harness)
            .and_then(|info| {
                if info.last_used.elapsed() < self.session_ttl {
                    Some(info.session_id.as_str())
                } else {
                    None
                }
            })
    }

    pub(crate) fn set_session_id(&mut self, harness: Harness, session_id: String) {
        self.sessions_by_harness.insert(
            harness,
            SessionInfo {
                session_id,
                last_used: Instant::now(),
            },
        );
    }

    /// Update the last used time for a session (simulates acpx session owner activity)
    pub(crate) fn touch_session(&mut self, harness: Harness) {
        if let Some(info) = self.sessions_by_harness.get_mut(&harness) {
            info.last_used = Instant::now();
        }
    }

    /// Clean up expired sessions based on TTL (acpx-style session owner cleanup)
    pub(crate) fn cleanup_expired_sessions(&mut self) {
        let now = Instant::now();
        self.sessions_by_harness
            .retain(|_, info| now.duration_since(info.last_used) < self.session_ttl);
    }

    /// Check if a session exists and is still valid (within TTL)
    pub(crate) fn has_valid_session(&self, harness: Harness) -> bool {
        self.session_id(harness).is_some()
    }

    /// Get the time remaining until a session expires
    pub(crate) fn session_time_remaining(&self, harness: Harness) -> Option<Duration> {
        self.sessions_by_harness
            .get(&harness)
            .and_then(|info| {
                let elapsed = info.last_used.elapsed();
                if elapsed < self.session_ttl {
                    Some(self.session_ttl - elapsed)
                } else {
                    None
                }
            })
    }

    pub(crate) fn last_harness(&self, conversation_id: AIConversationId) -> Option<Harness> {
        self.last_harness_by_conversation
            .get(&conversation_id)
            .copied()
    }

    pub(crate) fn set_last_harness(
        &mut self,
        conversation_id: AIConversationId,
        harness: Harness,
    ) {
        self.last_harness_by_conversation
            .insert(conversation_id, harness);
    }

    /// Get the configured TTL for sessions
    pub(crate) fn ttl(&self) -> Duration {
        self.session_ttl
    }

    /// Get the number of active sessions
    pub(crate) fn active_session_count(&self) -> usize {
        self.sessions_by_harness.len()
    }
}

impl Entity for LocalAcpSessionStore {
    type Event = ();
}

impl SingletonEntity for LocalAcpSessionStore {}

#[cfg(test)]
mod tests {
    use super::*;
    use warp_cli::agent::Harness;

    #[test]
    fn test_session_ttl() {
        let mut store = LocalAcpSessionStore {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: Duration::from_secs(300),
        };

        let harness = Harness::Claude;
        let session_id = "test-session-123".to_string();

        store.set_session_id(harness, session_id.clone());
        
        // Session should be valid immediately
        assert!(store.has_valid_session(harness));
        assert_eq!(store.session_id(harness), Some("test-session-123"));
        
        // Check time remaining
        let remaining = store.session_time_remaining(harness);
        assert!(remaining.is_some());
        assert!(remaining.unwrap() > Duration::from_secs(299));
    }

    #[test]
    fn test_session_expiration() {
        let mut store = LocalAcpSessionStore {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: Duration::from_millis(100), // Short TTL for testing
        };

        let harness = Harness::Codex;
        let session_id = "test-session-456".to_string();

        store.set_session_id(harness, session_id);
        
        // Session should be valid immediately
        assert!(store.has_valid_session(harness));
        
        // Wait for expiration
        std::thread::sleep(Duration::from_millis(150));
        
        // Session should now be expired
        assert!(!store.has_valid_session(harness));
        assert_eq!(store.session_id(harness), None);
    }

    #[test]
    fn test_touch_session() {
        let mut store = LocalAcpSessionStore {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: Duration::from_secs(300),
        };

        let harness = Harness::Gemini;
        let session_id = "test-session-789".to_string();

        store.set_session_id(harness, session_id);
        
        // Touch session should update last_used time
        std::thread::sleep(Duration::from_millis(10));
        store.touch_session(harness);
        
        // Session should still be valid
        assert!(store.has_valid_session(harness));
    }

    #[test]
    fn test_cleanup_expired_sessions() {
        let mut store = LocalAcpSessionStore {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: Duration::from_millis(100),
        };

        store.set_session_id(Harness::Claude, "session-1".to_string());
        store.set_session_id(Harness::Codex, "session-2".to_string());
        
        assert_eq!(store.active_session_count(), 2);
        
        // Wait for expiration
        std::thread::sleep(Duration::from_millis(150));
        
        store.cleanup_expired_sessions();
        
        // All sessions should be cleaned up
        assert_eq!(store.active_session_count(), 0);
    }

    #[test]
    fn test_last_harness_tracking() {
        let mut store = LocalAcpSessionStore {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
            session_ttl: Duration::from_secs(300),
        };

        let conversation_id = crate::ai::agent::conversation::AIConversationId::new();
        
        // Initially no harness
        assert!(store.last_harness(conversation_id).is_none());
        
        store.set_last_harness(conversation_id, Harness::Claude);
        assert_eq!(store.last_harness(conversation_id), Some(Harness::Claude));
        
        store.set_last_harness(conversation_id, Harness::Codex);
        assert_eq!(store.last_harness(conversation_id), Some(Harness::Codex));
    }
}
