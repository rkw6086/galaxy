use super::*;

#[test]
#[ignore = "CORE-3768 - need to clean up PREVIEW_FLAGS, but this is a temporary fix for the cluttered changelog"]
fn test_all_preview_flags_have_a_description() {
    for flag in PREVIEW_FLAGS {
        assert!(
            flag.flag_description()
                .is_some_and(|description| !description.is_empty()),
            "Missing description for preview-enabled flag {flag:?}"
        );
    }
}

#[test]
fn local_child_harnesses_are_local_only_by_default() {
    assert!(LOCAL_FLAGS.contains(&FeatureFlag::LocalClaudeCodexChildHarnesses));
    assert!(!DEBUG_FLAGS.contains(&FeatureFlag::LocalClaudeCodexChildHarnesses));
    assert!(!DOGFOOD_FLAGS.contains(&FeatureFlag::LocalClaudeCodexChildHarnesses));
}

#[test]
fn acp_backend_is_enabled_for_dogfood() {
    assert!(DOGFOOD_FLAGS.contains(&FeatureFlag::AgentClientProtocol));
    assert!(!PREVIEW_FLAGS.contains(&FeatureFlag::AgentClientProtocol));
    assert!(!RELEASE_FLAGS.contains(&FeatureFlag::AgentClientProtocol));
}

#[test]
fn dogfood_flags_do_not_enable_upstream_hosted_services() {
    for flag in [
        FeatureFlag::CreatingSharedSessions,
        FeatureFlag::AgentModeAnalytics,
        FeatureFlag::ProviderCommand,
        FeatureFlag::SummarizationViaMessageReplacement,
        FeatureFlag::GeminiNotifications,
        FeatureFlag::OzLaunchModal,
        FeatureFlag::WaitForEventsParentRegistration,
    ] {
        assert!(
            !DOGFOOD_FLAGS.contains(&flag),
            "{flag:?} depends on upstream-hosted or upstream-branded infrastructure"
        );
    }
}
