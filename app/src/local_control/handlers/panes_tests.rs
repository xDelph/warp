use ::local_control::{Action, ActionKind};
use serde_json::json;

use super::{limit_param, rename_param, text_param};

fn action_with_params(kind: ActionKind, params: serde_json::Value) -> Action {
    let mut action = Action::new(kind);
    action.params = params;
    action
}

#[test]
fn test_text_param_accepts_text() {
    let action = action_with_params(
        ActionKind::InputRun,
        json!({"type": "text", "text": "hello there"}),
    );
    assert_eq!(text_param(&action).unwrap(), "hello there");
}

#[test]
fn test_text_param_rejects_empty_params() {
    let action = Action::new(ActionKind::InputRun);
    assert!(text_param(&action).is_err());
}

#[test]
fn test_limit_param_defaults_to_none() {
    let action = Action::new(ActionKind::BlockOutput);
    assert_eq!(limit_param(&action).unwrap(), None);
}

#[test]
fn test_limit_param_parses_limit() {
    let action = action_with_params(
        ActionKind::BlockOutput,
        json!({"type": "limit", "limit": 42}),
    );
    assert_eq!(limit_param(&action).unwrap(), Some(42));
}

#[test]
fn test_rename_param_parses_title() {
    let action = action_with_params(
        ActionKind::TabRename,
        json!({"type": "rename", "title": "devin-implementer"}),
    );
    assert_eq!(rename_param(&action).unwrap(), "devin-implementer");
}
