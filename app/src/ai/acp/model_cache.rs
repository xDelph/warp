//! Disk cache for discovered local ACP models.
//!
//! Model discovery requires a full agent subprocess handshake (spawn →
//! `initialize` → `session/new`), which can take multiple seconds per harness.
//! Caching the last successful discovery lets the model picker populate
//! instantly on every launch after the first, while a background refresh keeps
//! the list current.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use warp_cli::agent::Harness;

use super::models::LocalAcpModelInfo;

const CACHE_FILE_NAME: &str = "local_acp_model_cache.json";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, Serialize, Deserialize)]
struct ModelCacheFile {
    harnesses: HashMap<String, CachedHarnessModels>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedHarnessModels {
    fetched_at_unix_secs: u64,
    models: Vec<CachedModel>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedModel {
    id: String,
    name: String,
}

/// acpx-compatible model cache interface
pub struct ModelCache;

impl ModelCache {
    /// Try to get cached models for a harness (acpx pattern)
    pub fn try_get_cached_models(harness: Harness) -> Option<Vec<LocalAcpModelInfo>> {
        load_fresh(harness)
    }

    /// Cache models for a harness (acpx pattern)
    pub fn cache_models(harness: Harness, models: Vec<LocalAcpModelInfo>) {
        store(harness, &models);
    }
}

fn cache_path() -> PathBuf {
    warp_core::paths::state_dir().join(CACHE_FILE_NAME)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn read_cache_file(path: &Path) -> ModelCacheFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub(crate) fn load_fresh(harness: Harness) -> Option<Vec<LocalAcpModelInfo>> {
    load_fresh_from(&cache_path(), harness, now_unix_secs())
}

fn load_fresh_from(path: &Path, harness: Harness, now: u64) -> Option<Vec<LocalAcpModelInfo>> {
    let cache = read_cache_file(path);
    let entry = cache.harnesses.get(harness.config_name())?;
    if now.saturating_sub(entry.fetched_at_unix_secs) > CACHE_TTL.as_secs() {
        return None;
    }
    if entry.models.is_empty() {
        return None;
    }
    Some(
        entry
            .models
            .iter()
            .map(|model| LocalAcpModelInfo {
                id: model.id.clone(),
                name: model.name.clone(),
            })
            .collect(),
    )
}

pub(crate) fn store(harness: Harness, models: &[LocalAcpModelInfo]) {
    store_in(&cache_path(), harness, models, now_unix_secs());
}

fn store_in(path: &Path, harness: Harness, models: &[LocalAcpModelInfo], now: u64) {
    if models.is_empty() {
        return;
    }
    let mut cache = read_cache_file(path);
    cache.harnesses.insert(
        harness.config_name().to_string(),
        CachedHarnessModels {
            fetched_at_unix_secs: now,
            models: models
                .iter()
                .map(|model| CachedModel {
                    id: model.id.clone(),
                    name: model.name.clone(),
                })
                .collect(),
        },
    );

    let write_result = serde_json::to_string_pretty(&cache).map(|contents| {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, contents)
    });
    if let Ok(Err(error)) | Err(error) = write_result.map_err(std::io::Error::other) {
        log::warn!("failed to write local ACP model cache: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_models() -> Vec<LocalAcpModelInfo> {
        vec![
            LocalAcpModelInfo {
                id: "gpt-5.5".to_string(),
                name: "GPT-5.5".to_string(),
            },
            LocalAcpModelInfo {
                id: "gpt-5.4".to_string(),
                name: "GPT-5.4".to_string(),
            },
        ]
    }

    #[test]
    fn stores_and_loads_fresh_models() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        store_in(&path, Harness::Codex, &sample_models(), 1_000);

        assert_eq!(
            load_fresh_from(&path, Harness::Codex, 1_000 + CACHE_TTL.as_secs()),
            Some(sample_models())
        );
        assert_eq!(load_fresh_from(&path, Harness::Claude, 1_000), None);
    }

    #[test]
    fn expired_entries_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        store_in(&path, Harness::Codex, &sample_models(), 1_000);

        assert_eq!(
            load_fresh_from(&path, Harness::Codex, 1_001 + CACHE_TTL.as_secs()),
            None
        );
    }

    #[test]
    fn empty_model_lists_are_not_cached() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        store_in(&path, Harness::Codex, &[], 1_000);

        assert!(!path.exists());
        assert_eq!(load_fresh_from(&path, Harness::Codex, 1_000), None);
    }

    #[test]
    fn corrupt_cache_is_treated_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        std::fs::write(&path, "not json").unwrap();

        assert_eq!(load_fresh_from(&path, Harness::Codex, 1_000), None);
        store_in(&path, Harness::Codex, &sample_models(), 1_000);
        assert_eq!(
            load_fresh_from(&path, Harness::Codex, 1_000),
            Some(sample_models())
        );
    }
}
