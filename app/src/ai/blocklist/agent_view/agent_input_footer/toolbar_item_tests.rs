use super::AgentToolbarItemKind;

#[test]
fn legacy_share_session_setting_remains_deserializable() {
    let item: AgentToolbarItemKind =
        serde_json::from_str("\"ShareSession\"").expect("legacy setting should deserialize");

    assert_eq!(item, AgentToolbarItemKind::ShareSession);
}

#[test]
fn share_session_is_not_offered_by_defaults_or_configurator() {
    assert!(!AgentToolbarItemKind::default_right().contains(&AgentToolbarItemKind::ShareSession));
    assert!(!AgentToolbarItemKind::all_available().contains(&AgentToolbarItemKind::ShareSession));
}
