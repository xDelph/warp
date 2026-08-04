//! acpx-style session owner architecture for ACP sessions.
//!
//! This module implements acpx's detached session owner pattern where a
//! background process keeps ACP sessions warm between prompts, avoiding
//! cold boot costs. The owner process maintains session state with a TTL
//! and handles queue management for concurrent prompt submissions.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use warp_cli::agent::Harness;
use warpui::{Entity, ModelContext, SingletonEntity};

/// Default TTL for session owner (matches acpx default of 300s)
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(300);

/// acpx session owner that keeps ACP sessions warm
pub struct AcpxSessionOwner {
    active_sessions: Arc<Mutex<Vec<ActiveSession>>>,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct ActiveSession {
    harness: Harness,
    session_id: String,
    last_activity: Instant,
    pid: u32,
}

impl AcpxSessionOwner {
    pub fn new(_: &mut ModelContext<Self>) -> Self {
        Self {
            active_sessions: Arc::new(Mutex::new(Vec::new())),
            ttl: DEFAULT_SESSION_TTL,
        }
    }

    /// Register a session as active (simulates acpx session owner)
    pub fn register_session(&self, harness: Harness, session_id: String, pid: u32) {
        let mut sessions = self.active_sessions.lock().unwrap();
        // Remove any existing session for this harness to avoid duplicates
        sessions.retain(|s| s.harness != harness);
        sessions.push(ActiveSession {
            harness,
            session_id,
            last_activity: Instant::now(),
            pid,
        });
    }

    /// Mark a session as active (called on each prompt)
    pub fn touch_session(&self, harness: Harness, session_id: &str) {
        let mut sessions = self.active_sessions.lock().unwrap();
        if let Some(session) = sessions.iter_mut().find(|s| s.harness == harness && s.session_id == session_id) {
            session.last_activity = Instant::now();
        }
    }

    /// Clean up expired sessions (acpx TTL-based cleanup)
    pub fn cleanup_expired_sessions(&self) {
        let mut sessions = self.active_sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|session| now.duration_since(session.last_activity) < self.ttl);
    }

    /// Check if a session is active and warm
    pub fn is_session_warm(&self, harness: Harness, session_id: &str) -> bool {
        let sessions = self.active_sessions.lock().unwrap();
        sessions
            .iter()
            .any(|s| s.harness == harness && s.session_id == session_id && s.last_activity.elapsed() < self.ttl)
    }

    /// Get the number of active sessions
    pub fn active_session_count(&self) -> usize {
        let sessions = self.active_sessions.lock().unwrap();
        sessions.len()
    }

    /// Get the TTL for sessions
    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

impl Entity for AcpxSessionOwner {
    type Event = ();
}

impl SingletonEntity for AcpxSessionOwner {}