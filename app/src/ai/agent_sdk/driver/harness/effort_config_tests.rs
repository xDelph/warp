//! Comprehensive tests for effort configuration functionality.
//!
//! Tests cover:
//! - Storing and retrieving effort configuration
//! - Persistence across restarts
//! - Different effort levels being correctly passed
//! - Default behavior when effort is not configured
//! - Serialization/Deserialization
//! - Edge cases

use std::fs;

use cloud_object_models::HarnessModelConfig;
use tempfile::TempDir;

/// Helper function to create a HarnessModelConfig with effort level
fn harness_model_config_with_effort(model_id: &str, reasoning_level: Option<&str>) -> HarnessModelConfig {
    HarnessModelConfig {
        model_id: model_id.to_string(),
        reasoning_level: reasoning_level.map(str::to_string),
    }
}

// ============================================================================
// Storing and Retrieving Effort Configuration
// ============================================================================

#[test]
fn effort_config_stores_low_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("low"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("low".to_string()));
}

#[test]
fn effort_config_stores_medium_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("medium"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("medium".to_string()));
}

#[test]
fn effort_config_stores_high_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_stores_xhigh_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("xhigh"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("xhigh".to_string()));
}

#[test]
fn effort_config_stores_max_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("max"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("max".to_string()));
}

#[test]
fn effort_config_with_none_reasoning_level() {
    let config = harness_model_config_with_effort("gpt-5.5", None);
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, None);
}

#[test]
fn effort_config_retrieves_correct_level() {
    let config = harness_model_config_with_effort("claude-opus-4-7", Some("high"));
    assert_eq!(config.reasoning_level.as_deref(), Some("high"));
}

// ============================================================================
// Serialization/Deserialization
// ============================================================================

#[test]
fn effort_config_serializes_correctly() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let serialized = serde_json::to_string(&config).unwrap();
    let deserialized: HarnessModelConfig = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_deserializes_with_none() {
    let json = r#"{"model_id":"gpt-5.5"}"#;
    let deserialized: HarnessModelConfig = serde_json::from_str(json).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, None);
}

#[test]
fn effort_config_deserializes_with_effort() {
    let json = r#"{"model_id":"gpt-5.5","reasoning_level":"medium"}"#;
    let deserialized: HarnessModelConfig = serde_json::from_str(json).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, Some("medium".to_string()));
}

#[test]
fn effort_config_toml_serialization() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let serialized = toml::to_string(&config).unwrap();
    let deserialized: HarnessModelConfig = toml::from_str(&serialized).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_toml_round_trip_all_levels() {
    for level in ["low", "medium", "high", "xhigh", "max"] {
        let config = harness_model_config_with_effort("gpt-5.5", Some(level));
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: HarnessModelConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.model_id, "gpt-5.5");
        assert_eq!(deserialized.reasoning_level, Some(level.to_string()));
    }
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn effort_config_handles_special_characters_in_model_id() {
    let config = harness_model_config_with_effort("model-with_underscores", Some("high"));
    assert_eq!(config.model_id, "model-with_underscores");
    assert_eq!(config.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_handles_very_long_model_ids() {
    let long_id = "a".repeat(1000);
    let config = harness_model_config_with_effort(&long_id, Some("medium"));
    assert_eq!(config.model_id, long_id);
    assert_eq!(config.reasoning_level, Some("medium".to_string()));
}

#[test]
fn effort_config_handles_unicode_in_model_id() {
    let config = harness_model_config_with_effort("model-ñ-中文", Some("low"));
    assert_eq!(config.model_id, "model-ñ-中文");
    assert_eq!(config.reasoning_level, Some("low".to_string()));
}

#[test]
fn effort_config_case_sensitivity() {
    let config1 = harness_model_config_with_effort("gpt-5.5", Some("High"));
    let config2 = harness_model_config_with_effort("gpt-5.5", Some("high"));

    // Effort levels should be case-sensitive
    assert_eq!(config1.reasoning_level, Some("High".to_string()));
    assert_eq!(config2.reasoning_level, Some("high".to_string()));
    assert_ne!(config1.reasoning_level, config2.reasoning_level);
}

#[test]
fn effort_config_empty_string_reasoning_level() {
    let config = harness_model_config_with_effort("gpt-5.5", Some(""));
    // Empty string is still Some(""), not None
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("".to_string()));
}

#[test]
fn effort_config_clones_correctly() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let cloned = config.clone();

    assert_eq!(cloned.model_id, "gpt-5.5");
    assert_eq!(cloned.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_equality() {
    let config1 = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let config2 = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let config3 = harness_model_config_with_effort("gpt-5.5", Some("medium"));

    assert_eq!(config1, config2);
    assert_ne!(config1, config3);
}

// ============================================================================
// Integration with Different Models
// ============================================================================

#[test]
fn effort_config_works_with_gpt_models() {
    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_works_with_claude_models() {
    let config = harness_model_config_with_effort("claude-opus-4-7", Some("max"));
    assert_eq!(config.model_id, "claude-opus-4-7");
    assert_eq!(config.reasoning_level, Some("max".to_string()));
}

#[test]
fn effort_config_works_with_custom_model_ids() {
    let config = harness_model_config_with_effort("custom/model-123", Some("medium"));
    assert_eq!(config.model_id, "custom/model-123");
    assert_eq!(config.reasoning_level, Some("medium".to_string()));
}

// ============================================================================
// Default Behavior
// ============================================================================

#[test]
fn effort_config_defaults_to_none_when_not_specified() {
    let config = harness_model_config_with_effort("gpt-5.5", None);
    assert_eq!(config.reasoning_level, None);
}

#[test]
fn harness_model_config_from_harness_config_with_effort() {
    use cloud_object_models::HarnessConfig;
    use warp_cli::agent::Harness;

    let harness_config = HarnessConfig {
        harness_type: Harness::Codex,
        model_id: Some("gpt-5.5".to_string()),
        reasoning_level: Some("high".to_string()),
    };

    let model_config = harness_config.model_config();
    assert!(model_config.is_some());
    let config = model_config.unwrap();
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, Some("high".to_string()));
}

#[test]
fn harness_model_config_from_harness_config_without_effort() {
    use cloud_object_models::HarnessConfig;
    use warp_cli::agent::Harness;

    let harness_config = HarnessConfig {
        harness_type: Harness::Codex,
        model_id: Some("gpt-5.5".to_string()),
        reasoning_level: None,
    };

    let model_config = harness_config.model_config();
    assert!(model_config.is_some());
    let config = model_config.unwrap();
    assert_eq!(config.model_id, "gpt-5.5");
    assert_eq!(config.reasoning_level, None);
}

#[test]
fn harness_model_config_from_harness_config_without_model() {
    use cloud_object_models::HarnessConfig;
    use warp_cli::agent::Harness;

    let harness_config = HarnessConfig {
        harness_type: Harness::Codex,
        model_id: None,
        reasoning_level: Some("high".to_string()),
    };

    let model_config = harness_config.model_config();
    assert!(model_config.is_none());
}

// ============================================================================
// Persistence Tests (File-based)
// ============================================================================

#[test]
fn effort_config_persists_to_json_file() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.json");

    let config = harness_model_config_with_effort("gpt-5.5", Some("high"));
    let json = serde_json::to_string_pretty(&config).unwrap();
    fs::write(&config_path, json).unwrap();

    let read_back = fs::read_to_string(&config_path).unwrap();
    let deserialized: HarnessModelConfig = serde_json::from_str(&read_back).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, Some("high".to_string()));
}

#[test]
fn effort_config_persists_to_toml_file() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let config = harness_model_config_with_effort("gpt-5.5", Some("medium"));
    let toml_str = toml::to_string_pretty(&config).unwrap();
    fs::write(&config_path, toml_str).unwrap();

    let read_back = fs::read_to_string(&config_path).unwrap();
    let deserialized: HarnessModelConfig = toml::from_str(&read_back).unwrap();

    assert_eq!(deserialized.model_id, "gpt-5.5");
    assert_eq!(deserialized.reasoning_level, Some("medium".to_string()));
}

#[test]
fn effort_config_file_persistence_across_reads() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.json");

    // Write initial config
    let config1 = harness_model_config_with_effort("gpt-5.5", Some("low"));
    let json1 = serde_json::to_string(&config1).unwrap();
    fs::write(&config_path, json1).unwrap();

    // Read it back
    let read_back = fs::read_to_string(&config_path).unwrap();
    let deserialized: HarnessModelConfig = serde_json::from_str(&read_back).unwrap();

    // Write it again (simulating a restart)
    let json2 = serde_json::to_string(&deserialized).unwrap();
    fs::write(&config_path, json2).unwrap();

    // Read again and verify
    let final_read = fs::read_to_string(&config_path).unwrap();
    let final_config: HarnessModelConfig = serde_json::from_str(&final_read).unwrap();

    assert_eq!(final_config.model_id, "gpt-5.5");
    assert_eq!(final_config.reasoning_level, Some("low".to_string()));
}

// ============================================================================
// Valid Effort Levels
// ============================================================================

#[test]
fn all_valid_effort_levels_are_storable() {
    let valid_levels = ["low", "medium", "high", "xhigh", "max"];

    for level in valid_levels {
        let config = harness_model_config_with_effort("gpt-5.5", Some(level));
        assert_eq!(config.reasoning_level, Some(level.to_string()));
    }
}

#[test]
fn custom_effort_level_is_storable() {
    // The system allows custom effort levels (strings)
    let config = harness_model_config_with_effort("gpt-5.5", Some("custom-level"));
    assert_eq!(config.reasoning_level, Some("custom-level".to_string()));
}

#[test]
fn numeric_effort_level_is_storable() {
    // Even numeric strings are allowed
    let config = harness_model_config_with_effort("gpt-5.5", Some("1"));
    assert_eq!(config.reasoning_level, Some("1".to_string()));
}

// ============================================================================
// ModelEffort Enum Tests
// ============================================================================

#[test]
fn model_effort_enum_conversion() {
    use cloud_object_models::ModelEffort;

    assert_eq!(ModelEffort::Low.as_str(), "low");
    assert_eq!(ModelEffort::Medium.as_str(), "medium");
    assert_eq!(ModelEffort::High.as_str(), "high");
}

#[test]
fn model_effort_from_str() {
    use cloud_object_models::ModelEffort;

    assert_eq!(ModelEffort::from_str("low"), Some(ModelEffort::Low));
    assert_eq!(ModelEffort::from_str("medium"), Some(ModelEffort::Medium));
    assert_eq!(ModelEffort::from_str("high"), Some(ModelEffort::High));
    assert_eq!(ModelEffort::from_str("invalid"), None);
}

#[test]
fn model_effort_display_name() {
    use cloud_object_models::ModelEffort;

    assert_eq!(ModelEffort::Low.display_name(), "Low");
    assert_eq!(ModelEffort::Medium.display_name(), "Medium");
    assert_eq!(ModelEffort::High.display_name(), "High");
}

#[test]
fn model_effort_default() {
    use cloud_object_models::ModelEffort;

    assert_eq!(ModelEffort::default(), ModelEffort::Medium);
}
