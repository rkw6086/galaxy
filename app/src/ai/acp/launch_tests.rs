use super::*;

#[test]
fn unknown_builtin_agent_ids_are_rejected() {
    let error = resolve_acp_launch("mystery-agent", "", &[]).unwrap_err();

    assert!(error.contains("Unknown ACP agent preset"));
}

#[test]
fn custom_agent_ids_are_allowed_with_an_explicit_executable() {
    let executable = std::env::current_exe().unwrap();
    let launch = resolve_acp_launch(
        "my-agent",
        executable.to_str().unwrap(),
        &["--acp".to_owned()],
    )
    .unwrap();

    assert_eq!(launch.command, executable);
    assert_eq!(launch.args, vec!["--acp"]);
}

#[test]
fn acp_model_identity_names_the_agent_instead_of_a_provider_model() {
    assert_eq!(acp_model_id(" Codex "), "acp:codex");
    assert_eq!(acp_model_id("My-Agent"), "acp:my-agent");
}

#[test]
fn launch_fingerprint_is_stable_and_sensitive_to_the_full_configuration() {
    let executable = std::env::current_exe().unwrap();
    let command = executable.to_str().unwrap();
    let launch = AcpLaunchConfig::new(&executable)
        .args(["serve"])
        .env("INITIAL_AGENT_MODE", "read-only")
        .preferred_auth_method("browser");
    let baseline = effective_launch_fingerprint("custom", None, &launch, true);

    assert_eq!(
        baseline,
        effective_launch_fingerprint("CUSTOM", None, &launch, true)
    );
    assert_ne!(
        baseline,
        effective_launch_fingerprint("custom", None, &launch.clone().args(["other"]), true,)
    );
    assert_ne!(
        baseline,
        effective_launch_fingerprint(
            "custom",
            None,
            &launch.clone().env("INITIAL_AGENT_MODE", "workspace-write"),
            true,
        )
    );
    assert_ne!(
        baseline,
        effective_launch_fingerprint(
            "custom",
            None,
            &launch.clone().preferred_auth_method("api-key"),
            true,
        )
    );
    assert_eq!(
        acp_launch_fingerprint("custom", command, &["serve".to_owned()]),
        acp_launch_fingerprint("CUSTOM", command, &["serve".to_owned()])
    );
}

#[test]
fn builtin_presets_ignore_custom_arguments_but_include_the_pinned_version() {
    assert_eq!(
        acp_launch_fingerprint("codex", "", &["ignored".to_owned()]),
        acp_launch_fingerprint("codex", "", &["also-ignored".to_owned()])
    );

    let launch = AcpAgentPreset::Codex.launch_config();
    assert_ne!(
        effective_launch_fingerprint("codex", Some(CODEX_ACP_NPM_VERSION), &launch, false),
        effective_launch_fingerprint("codex", Some("different-version"), &launch, false)
    );
}

#[test]
fn persisted_sessions_require_the_same_launch_identity() {
    let executable = std::env::current_exe().unwrap();
    let command = executable.to_str().unwrap();
    let args = vec!["serve".to_owned()];
    let launch = resolve_acp_launch("custom", command, &args).unwrap();
    let backend = AcpConversationData {
        agent_id: "custom".to_owned(),
        launch_fingerprint: acp_launch_fingerprint("custom", command, &args),
        session_id: Some("session-123".to_owned()),
    };

    assert!(validate_acp_launch_identity(&backend, "custom", command, &launch).is_ok());
    assert!(
        validate_acp_launch_identity(&backend, "other", command, &launch)
            .unwrap_err()
            .contains("configuration has changed")
    );
    let different_launch = launch.clone().args(["other"]);
    assert!(
        validate_acp_launch_identity(&backend, "custom", command, &different_launch)
            .unwrap_err()
            .contains("configuration has changed")
    );
}

#[test]
fn legacy_acp_sessions_fail_closed_without_a_launch_fingerprint() {
    let backend = AcpConversationData {
        agent_id: "codex".to_owned(),
        launch_fingerprint: String::new(),
        session_id: Some("legacy-session".to_owned()),
    };

    let launch = AcpAgentPreset::Codex.launch_config();
    assert!(validate_acp_launch_identity(&backend, "codex", "", &launch)
        .unwrap_err()
        .contains("predates"));
}

#[test]
fn persisted_acp_conversations_respect_the_runtime_toggle() {
    let error = validate_acp_dispatch(true, false, false).unwrap_err();

    assert!(error.contains("currently disabled"));
}

#[test]
fn local_acp_processes_cannot_target_remote_terminals() {
    let error = validate_acp_dispatch(true, true, true).unwrap_err();

    assert!(error.contains("Wormhole or SSH"));
    assert!(error.contains("start a new provider-backed conversation"));
    assert!(validate_acp_dispatch(true, true, false).is_ok());
}
