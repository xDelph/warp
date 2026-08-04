//! Reusable command-agent pane naming allocator.
//!
//! Provides a pure API for allocating and managing numbered pane names
//! (e.g., codex-1, codex-2) with suffix reuse after true close and
//! suffix preservation while detached.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uuid;

/// Global pane name allocator instance.
static GLOBAL_ALLOCATOR: std::sync::OnceLock<PaneNameAllocator> = std::sync::OnceLock::new();

/// Returns the global pane name allocator instance.
pub fn global_allocator() -> &'static PaneNameAllocator {
    GLOBAL_ALLOCATOR.get_or_init(PaneNameAllocator::new)
}

/// Allocation state for a single base command name.
#[derive(Clone, Debug, Default)]
struct AllocationState {
    /// Currently allocated suffixes (both active and detached).
    allocated: HashSet<u32>,
    /// Next suffix to try for new allocations.
    next_suffix: u32,
}

/// Pure allocator for command-agent pane naming.
///
/// Thread-safe singleton that tracks allocation state per base command name.
#[derive(Clone)]
pub struct PaneNameAllocator {
    inner: Arc<Mutex<HashMap<String, AllocationState>>>,
}

impl Default for PaneNameAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneNameAllocator {
    /// Creates a new allocator.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Allocates a new pane name for the given base command.
    ///
    /// Returns a name like "codex-1", "codex-2", etc.
    /// Reuses the lowest available suffix after a true close.
    pub fn allocate(&self, base_command: &str) -> String {
        let mut inner = self.inner.lock().unwrap();
        let state = inner.entry(base_command.to_string()).or_default();

        // Find the lowest unallocated suffix
        let suffix = (1..)
            .find(|n| !state.allocated.contains(n))
            .unwrap_or_else(|| {
                // If all 1..next_suffix are allocated, use next_suffix
                let suffix = state.next_suffix;
                state.next_suffix += 1;
                suffix
            });

        state.allocated.insert(suffix);
        format!("{}-{}", base_command, suffix)
    }

    /// Marks a pane name as allocated (for restoration from persistent state).
    ///
    /// Used when restoring panes from app-state to avoid name conflicts.
    pub fn mark_allocated(&self, name: &str) {
        if let Some((base, suffix)) = parse_pane_name(name) {
            let mut inner = self.inner.lock().unwrap();
            let state = inner.entry(base).or_default();
            state.allocated.insert(suffix);
            // Update next_suffix if this suffix is >= current next_suffix
            if suffix >= state.next_suffix {
                state.next_suffix = suffix + 1;
            }
        }
    }

    /// Releases a pane name allocation (true close).
    ///
    /// The suffix becomes available for reuse on the next allocation.
    pub fn release(&self, name: &str) {
        if let Some((base, suffix)) = parse_pane_name(name) {
            let mut inner = self.inner.lock().unwrap();
            if let Some(state) = inner.get_mut(&base) {
                state.allocated.remove(&suffix);
            }
        }
    }

    /// Returns all currently allocated names for a base command.
    pub fn list_allocated(&self, base_command: &str) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        if let Some(state) = inner.get(base_command) {
            state
                .allocated
                .iter()
                .map(|&suffix| format!("{}-{}", base_command, suffix))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Returns the count of allocated panes for a base command.
    pub fn count_allocated(&self, base_command: &str) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .get(base_command)
            .map(|state| state.allocated.len())
            .unwrap_or(0)
    }

    /// Allocates a pane name for RMUX sessions when a base command is known.
    ///
    /// This is a convenience method for use in RMUX pane creation where the
    /// base command (e.g., "codex", "claude") is available. Falls back to
    /// UUID-based naming if no base command is provided.
    pub fn allocate_rmux_session_name(&self, base_command: Option<&str>) -> String {
        match base_command {
            Some(base) => self.allocate(base),
            None => format!("warp-{}", uuid::Uuid::new_v4()),
        }
    }
}

/// Parses a pane name into (base_command, suffix).
///
/// Returns None if the name doesn't match the "base-N" pattern.
fn parse_pane_name(name: &str) -> Option<(String, u32)> {
    let (base, suffix) = name.rsplit_once('-')?;
    if base.is_empty() || base.contains('-') {
        return None;
    }
    Some((base.to_string(), suffix.parse::<u32>().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_first() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.allocate("codex"), "codex-1");
    }

    #[test]
    fn test_allocate_sequence() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.allocate("codex"), "codex-1");
        assert_eq!(allocator.allocate("codex"), "codex-2");
        assert_eq!(allocator.allocate("codex"), "codex-3");
    }

    #[test]
    fn test_release_reuse_lowest() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.allocate("codex"), "codex-1");
        assert_eq!(allocator.allocate("codex"), "codex-2");
        assert_eq!(allocator.allocate("codex"), "codex-3");

        allocator.release("codex-2");
        assert_eq!(allocator.allocate("codex"), "codex-2"); // Reuses lowest
    }

    #[test]
    fn test_release_middle_reuses_lowest() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.allocate("codex"), "codex-1");
        assert_eq!(allocator.allocate("codex"), "codex-2");
        assert_eq!(allocator.allocate("codex"), "codex-3");

        allocator.release("codex-1");
        assert_eq!(allocator.allocate("codex"), "codex-1"); // Reuses lowest
    }

    #[test]
    fn test_mark_allocated_restores_state() {
        let allocator = PaneNameAllocator::new();
        allocator.mark_allocated("codex-1");
        allocator.mark_allocated("codex-3");

        assert_eq!(allocator.allocate("codex"), "codex-2"); // Fills gap
        assert_eq!(allocator.allocate("codex"), "codex-4"); // Next after 3
    }

    #[test]
    fn test_multiple_base_commands() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.allocate("codex"), "codex-1");
        assert_eq!(allocator.allocate("claude"), "claude-1");
        assert_eq!(allocator.allocate("codex"), "codex-2");
        assert_eq!(allocator.allocate("claude"), "claude-2");
    }

    #[test]
    fn test_list_allocated() {
        let allocator = PaneNameAllocator::new();
        allocator.allocate("codex");
        allocator.allocate("codex");
        allocator.allocate("claude");

        let codex_names = allocator.list_allocated("codex");
        assert_eq!(codex_names.len(), 2);
        assert!(codex_names.contains(&"codex-1".to_string()));
        assert!(codex_names.contains(&"codex-2".to_string()));

        let claude_names = allocator.list_allocated("claude");
        assert_eq!(claude_names.len(), 1);
        assert!(claude_names.contains(&"claude-1".to_string()));
    }

    #[test]
    fn test_count_allocated() {
        let allocator = PaneNameAllocator::new();
        assert_eq!(allocator.count_allocated("codex"), 0);

        allocator.allocate("codex");
        allocator.allocate("codex");
        assert_eq!(allocator.count_allocated("codex"), 2);

        allocator.release("codex-1");
        assert_eq!(allocator.count_allocated("codex"), 1);
    }

    #[test]
    fn test_parse_pane_name_valid() {
        assert_eq!(parse_pane_name("codex-1"), Some(("codex".to_string(), 1)));
        assert_eq!(parse_pane_name("claude-42"), Some(("claude".to_string(), 42)));
        assert_eq!(parse_pane_name("agent-999"), Some(("agent".to_string(), 999)));
    }

    #[test]
    fn test_parse_pane_name_invalid() {
        assert_eq!(parse_pane_name("codex"), None);
        assert_eq!(parse_pane_name("codex-abc"), None);
        assert_eq!(parse_pane_name("codex-1-2"), None);
        assert_eq!(parse_pane_name(""), None);
    }

    #[test]
    fn test_release_invalid_name_no_panic() {
        let allocator = PaneNameAllocator::new();
        allocator.allocate("codex");
        allocator.release("invalid-name"); // Should not panic
        assert_eq!(allocator.count_allocated("codex"), 1);
    }

    #[test]
    fn test_mark_allocated_invalid_name_no_panic() {
        let allocator = PaneNameAllocator::new();
        allocator.mark_allocated("invalid-name"); // Should not panic
        assert_eq!(allocator.count_allocated("codex"), 0);
    }
}
