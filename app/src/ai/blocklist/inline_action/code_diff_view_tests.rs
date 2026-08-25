use super::*;

#[test]
fn blocked_action_status_makes_queued_diff_actionable() {
    let mut state = CodeDiffState::Queued;

    state.sync_with_action_status(Some(&AIActionStatus::Blocked));

    assert!(state.is_waiting_for_user());
}

#[test]
fn preprocessing_action_status_keeps_diff_queued() {
    let mut state = CodeDiffState::Queued;

    state.sync_with_action_status(Some(&AIActionStatus::Preprocessing));

    assert!(matches!(state, CodeDiffState::Queued));
}
