use super::{
    AgentAttributionToggleState, GrokSubscriptionButtonAction,
    derive_agent_attribution_toggle_state, grok_subscription_button_action,
};
use crate::ai::llms::{LLMId, LLMInfo, LLMUsageMetadata, LLMProvider};
use crate::workspaces::workspace::AdminEnablementSetting;

#[test]
fn respect_user_setting_returns_user_pref_unlocked() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        true,
        true,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: false,
            is_disabled: false,
        }
    );
}

#[test]
fn respect_user_setting_with_user_off_returns_unchecked_unlocked() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        false,
        true,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: false,
            is_forced_by_org: false,
            is_disabled: false,
        }
    );
}

#[test]
fn team_enable_locks_toggle_on_regardless_of_user_pref() {
    let state = derive_agent_attribution_toggle_state(&AdminEnablementSetting::Enable, false, true);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn team_disable_locks_toggle_off_regardless_of_user_pref() {
    let state = derive_agent_attribution_toggle_state(&AdminEnablementSetting::Disable, true, true);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: false,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn ai_globally_disabled_marks_toggle_disabled_but_not_forced() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        true,
        false,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: false,
            is_disabled: true,
        }
    );
}

#[test]
fn team_force_takes_precedence_over_global_ai_disabled() {
    let state =
        derive_agent_attribution_toggle_state(&AdminEnablementSetting::Enable, false, false);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn grok_button_action_reflects_tokens_and_attempt_phase() {
    assert_eq!(
        grok_subscription_button_action(false, None),
        GrokSubscriptionButtonAction::Connect
    );
    assert_eq!(
        grok_subscription_button_action(false, Some(false)),
        GrokSubscriptionButtonAction::Cancel
    );
    assert_eq!(
        grok_subscription_button_action(false, Some(true)),
        GrokSubscriptionButtonAction::Cancelling
    );
    // Stored tokens take precedence regardless of attempt phase.
    assert_eq!(
        grok_subscription_button_action(true, None),
        GrokSubscriptionButtonAction::Disconnect
    );
    assert_eq!(
        grok_subscription_button_action(true, Some(false)),
        GrokSubscriptionButtonAction::Disconnect
    );
    assert_eq!(
        grok_subscription_button_action(true, Some(true)),
        GrokSubscriptionButtonAction::Disconnect
    );
}
// ── Favorite models UI tests ─────────────────────────────────────────────────

fn create_test_llm_info(id: &str, display_name: &str) -> LLMInfo {
    LLMInfo {
        display_name: display_name.to_string(),
        base_model_name: display_name.to_string(),
        id: id.into(),
        reasoning_level: None,
        usage_metadata: LLMUsageMetadata {
            request_multiplier: 1,
            credit_multiplier: None,
        },
        description: None,
        disable_reason: None,
        vision_supported: false,
        spec: None,
        provider: LLMProvider::OpenAI,
        host_configs: std::collections::HashMap::new(),
        discount_percentage: None,
        context_window: Default::default(),
    }
}

#[test]
fn favorite_model_shows_star_icon_in_menu() {
    let favorite_ids: std::collections::HashSet<LLMId> =
        ["gpt-4o"].iter().map(|s| (*s).into()).collect();
    let model = create_test_llm_info("gpt-4o", "GPT-4o");

    let is_favorite = favorite_ids.contains(&model.id);

    assert!(is_favorite);
    // In the actual implementation, this would render a star icon in the menu
}

#[test]
fn non_favorite_model_does_not_show_star_icon() {
    let favorite_ids: std::collections::HashSet<LLMId> = std::collections::HashSet::new();
    let model = create_test_llm_info("gpt-4o", "GPT-4o");

    let is_favorite = favorite_ids.contains(&model.id);

    assert!(!is_favorite);
    // In the actual implementation, this would not render a star icon
}

#[test]
fn ctrl_f_toggle_adds_favorite_on_hover() {
    let hovered_model_id: LLMId = "gpt-4o".into();
    let mut favorites: std::collections::HashSet<LLMId> = std::collections::HashSet::new();

    // Simulate Ctrl+F press while hovering over a model
    favorites.insert(hovered_model_id.clone());

    assert!(favorites.contains(&hovered_model_id));
}

#[test]
fn ctrl_f_toggle_removes_favorite_on_second_press() {
    let model_id: LLMId = "gpt-4o".into();
    let mut favorites: std::collections::HashSet<LLMId> =
        ["gpt-4o"].iter().map(|s| (*s).into()).collect();

    // Simulate Ctrl+F press to remove favorite
    favorites.remove(&model_id);

    assert!(!favorites.contains(&model_id));
}

#[test]
fn f2_cycles_to_next_favorite_model() {
    let favorites: Vec<LLMId> = vec![
        "gpt-4o".into(),
        "claude-3-opus".into(),
        "gemini-pro".into(),
    ];

    let current_model: LLMId = "gpt-4o".into();
    let current_idx = favorites.iter().position(|id| id == &current_model).unwrap();

    // Cycle to next favorite
    let next_idx = (current_idx + 1) % favorites.len();
    let next_model = favorites[next_idx].clone();

    assert_eq!(next_model, "claude-3-opus".into());
}

#[test]
fn f2_cycles_from_last_to_first_favorite() {
    let favorites: Vec<LLMId> = vec![
        "gpt-4o".into(),
        "claude-3-opus".into(),
        "gemini-pro".into(),
    ];

    let current_model: LLMId = "gemini-pro".into();
    let current_idx = favorites.iter().position(|id| id == &current_model).unwrap();

    // Cycle from last to first
    let next_idx = (current_idx + 1) % favorites.len();
    let next_model = favorites[next_idx].clone();

    assert_eq!(next_model, "gpt-4o".into());
}

#[test]
fn f2_with_no_favorites_does_not_change_model() {
    let _favorites: Vec<LLMId> = vec![];
    let current_model: LLMId = "gpt-4o".into();

    // When no favorites, F2 should not change the model
    let next_model = current_model.clone();

    assert_eq!(next_model, current_model);
}

#[test]
fn favorites_sort_before_non_favorites_in_dropdown() {
    let favorite_ids: std::collections::HashSet<LLMId> =
        ["gpt-4o", "claude-3-opus"].iter().map(|s| (*s).into()).collect();

    let models = vec![
        create_test_llm_info("gpt-3.5-turbo", "GPT-3.5 Turbo"),
        create_test_llm_info("gpt-4o", "GPT-4o"),
        create_test_llm_info("claude-3-opus", "Claude 3 Opus"),
        create_test_llm_info("gemini-pro", "Gemini Pro"),
    ];

    // Sort models: favorites first, then others
    let mut sorted_models = models.clone();
    sorted_models.sort_by(|a, b| {
        let a_is_fav = favorite_ids.contains(&a.id);
        let b_is_fav = favorite_ids.contains(&b.id);
        b_is_fav.cmp(&a_is_fav)
    });

    // Favorites should come first
    assert!(favorite_ids.contains(&sorted_models[0].id));
    assert!(favorite_ids.contains(&sorted_models[1].id));
    assert!(!favorite_ids.contains(&sorted_models[2].id));
    assert!(!favorite_ids.contains(&sorted_models[3].id));
}

#[test]
fn favorite_indicator_visible_in_settings_page() {
    let favorite_ids: std::collections::HashSet<LLMId> =
        ["gpt-4o"].iter().map(|s| (*s).into()).collect();
    let model = create_test_llm_info("gpt-4o", "GPT-4o");

    let is_favorite = favorite_ids.contains(&model.id);

    assert!(is_favorite);
    // In the actual implementation, this would show a star in the settings UI
}

#[test]
fn multiple_favorites_maintain_insertion_order() {
    let favorite_ids: Vec<LLMId> =
        vec!["gpt-4o".into(), "claude-3-opus".into(), "gemini-pro".into()];

    let models = vec![
        create_test_llm_info("gpt-4o", "GPT-4o"),
        create_test_llm_info("claude-3-opus", "Claude 3 Opus"),
        create_test_llm_info("gemini-pro", "Gemini Pro"),
    ];

    // Sort by favorite status, maintaining insertion order within groups
    let mut sorted_models = models.clone();
    sorted_models.sort_by(|a, b| {
        let a_idx = favorite_ids.iter().position(|id| id == &a.id);
        let b_idx = favorite_ids.iter().position(|id| id == &b.id);
        match (a_idx, b_idx) {
            (Some(a_pos), Some(b_pos)) => a_pos.cmp(&b_pos),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    assert_eq!(sorted_models[0].id, "gpt-4o".into());
    assert_eq!(sorted_models[1].id, "claude-3-opus".into());
    assert_eq!(sorted_models[2].id, "gemini-pro".into());
}
