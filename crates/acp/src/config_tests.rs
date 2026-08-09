use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

#[test]
fn codex_preset_uses_the_adapter_with_npx() {
    let launch = AcpAgentPreset::Codex.launch_config();

    assert_eq!(launch.command, PathBuf::from("npx"));
    assert_eq!(
        launch.args,
        vec![
            "--yes",
            &format!("@agentclientprotocol/codex-acp@{CODEX_ACP_NPM_VERSION}")
        ]
    );
    assert_eq!(
        launch.env.get("DEFAULT_AUTH_REQUEST").map(String::as_str),
        Some(r#"{"methodId":"chat-gpt"}"#)
    );
    assert_eq!(
        launch.env.get("INITIAL_AGENT_MODE").map(String::as_str),
        Some("read-only")
    );
    assert_eq!(
        launch
            .preferred_auth_method
            .as_ref()
            .map(ToString::to_string),
        Some("chat-gpt".to_owned())
    );
}

#[test]
fn opencode_preset_is_version_pinned() {
    let launch = AcpAgentPreset::OpenCode.launch_config();

    assert_eq!(launch.command, PathBuf::from("npx"));
    assert_eq!(
        launch.args,
        vec![
            "--yes",
            &format!("opencode-ai@{OPENCODE_NPM_VERSION}"),
            "acp"
        ]
    );
}

#[test]
fn resolved_opencode_prefers_the_native_executable() {
    let launch = AcpAgentPreset::OpenCode
        .resolve_launch_config_with(|command| match command {
            "opencode" => Some(PathBuf::from("/opt/bin/opencode")),
            "npx" => Some(PathBuf::from("/opt/bin/npx")),
            _ => None,
        })
        .unwrap();

    assert_eq!(launch.command, PathBuf::from("/opt/bin/opencode"));
    assert_eq!(launch.args, vec!["acp"]);
}

#[test]
fn resolved_codex_uses_npx_adapter_and_local_cli() {
    let resolve = |command: &str| match command {
        "npx" => Some(PathBuf::from("/opt/bin/npx")),
        "codex" => Some(PathBuf::from("/opt/homebrew/bin/codex")),
        _ => None,
    };
    let codex = AcpAgentPreset::Codex
        .resolve_launch_config_with(resolve)
        .unwrap();

    assert_eq!(codex.command, PathBuf::from("/opt/bin/npx"));
    assert_eq!(
        codex.args,
        vec![
            "--yes".to_owned(),
            format!("@agentclientprotocol/codex-acp@{CODEX_ACP_NPM_VERSION}")
        ]
    );
    assert_eq!(
        codex.env.get("CODEX_PATH").map(String::as_str),
        Some("/opt/homebrew/bin/codex")
    );
    assert_eq!(
        codex.env.get("INITIAL_AGENT_MODE").map(String::as_str),
        Some("read-only")
    );
    assert_eq!(
        codex
            .preferred_auth_method
            .as_ref()
            .map(ToString::to_string),
        Some("chat-gpt".to_owned())
    );
}

#[test]
fn resolved_codex_falls_back_to_local_adapter_without_npx() {
    let resolve = |command: &str| match command {
        "codex" => Some(PathBuf::from("/opt/homebrew/bin/codex")),
        "codex-acp" => Some(PathBuf::from("/opt/bin/codex-acp")),
        _ => None,
    };
    let codex = AcpAgentPreset::Codex
        .resolve_launch_config_with(resolve)
        .unwrap();

    assert_eq!(codex.command, PathBuf::from("/opt/bin/codex-acp"));
    assert!(codex.args.is_empty());
    assert_eq!(
        codex.env.get("CODEX_PATH").map(String::as_str),
        Some("/opt/homebrew/bin/codex")
    );
}

#[test]
fn resolved_presets_explain_missing_launchers() {
    let error = AcpAgentPreset::Codex
        .resolve_launch_config_with(|_| None)
        .unwrap_err();

    assert!(error.contains("requires the locally installed codex CLI"));

    let opencode_error = AcpAgentPreset::OpenCode
        .resolve_launch_config_with(|command| {
            (command == "bunx").then(|| PathBuf::from("/opt/bin/bunx"))
        })
        .unwrap_err();
    assert!(opencode_error.contains("requires the opencode executable or npx"));

    let codex_adapter_error = AcpAgentPreset::Codex
        .resolve_launch_config_with(|command| {
            (command == "codex").then(|| PathBuf::from("/opt/homebrew/bin/codex"))
        })
        .unwrap_err();
    assert!(codex_adapter_error.contains("requires either a local codex-acp executable or npx"));
}

#[test]
fn launch_config_is_a_comparable_fingerprint() {
    let first = AcpLaunchConfig::new("/usr/bin/npx")
        .args(["agent", "acp"])
        .env("TOKEN", "first")
        .preferred_auth_method("browser");
    let same = first.clone();
    let different = first.clone().env("TOKEN", "second");
    let different_auth = first.clone().preferred_auth_method("api-key");

    assert_eq!(first, same);
    assert_ne!(first, different);
    assert_ne!(first, different_auth);
}

#[test]
fn child_environment_clears_credentials_and_preserves_runtime_paths() {
    let parent = [
        ("PATH", "/usr/bin"),
        ("HOME", "/Users/test"),
        ("XDG_CONFIG_HOME", "/Users/test/.config"),
        ("OPENAI_API_KEY", "secret-openai-key"),
        ("AWS_SECRET_ACCESS_KEY", "secret-aws-key"),
        ("HTTPS_PROXY", "https://user:secret@example.com"),
        ("GALAXY_INTERNAL_SECRET", "secret-galaxy-value"),
        ("XDG_AGENT_TOKEN", "secret-xdg-value"),
        ("LC_AGENT_TOKEN", "secret-locale-value"),
    ]
    .map(|(name, value)| (OsString::from(name), OsString::from(value)));
    let explicit = BTreeMap::from([
        ("INITIAL_AGENT_MODE".to_owned(), "read-only".to_owned()),
        (
            "DEFAULT_AUTH_REQUEST".to_owned(),
            r#"{"methodId":"chat-gpt"}"#.to_owned(),
        ),
    ]);

    let overrides = sanitized_environment_overrides(parent, &explicit);

    assert!(!overrides.contains_key("PATH"));
    assert!(!overrides.contains_key("HOME"));
    assert!(!overrides.contains_key("XDG_CONFIG_HOME"));
    assert_eq!(
        overrides.get("OPENAI_API_KEY").map(String::as_str),
        Some("")
    );
    assert_eq!(
        overrides.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
        Some("")
    );
    assert_eq!(overrides.get("HTTPS_PROXY").map(String::as_str), Some(""));
    assert_eq!(
        overrides.get("GALAXY_INTERNAL_SECRET").map(String::as_str),
        Some("")
    );
    assert_eq!(
        overrides.get("XDG_AGENT_TOKEN").map(String::as_str),
        Some("")
    );
    assert_eq!(
        overrides.get("LC_AGENT_TOKEN").map(String::as_str),
        Some("")
    );
    assert_eq!(
        overrides.get("INITIAL_AGENT_MODE").map(String::as_str),
        Some("read-only")
    );
    assert_eq!(
        overrides.get("DEFAULT_AUTH_REQUEST").map(String::as_str),
        Some(r#"{"methodId":"chat-gpt"}"#)
    );
}

#[test]
fn explicit_agent_environment_wins_over_scrubbing() {
    let parent = [(
        OsString::from("AGENT_AUTH_TOKEN"),
        OsString::from("parent-secret"),
    )];
    let explicit = BTreeMap::from([("AGENT_AUTH_TOKEN".to_owned(), "explicit-value".to_owned())]);

    let overrides = sanitized_environment_overrides(parent, &explicit);

    assert_eq!(
        overrides.get("AGENT_AUTH_TOKEN").map(String::as_str),
        Some("explicit-value")
    );
}

#[test]
fn configured_commands_are_resolved_before_launch() {
    let executable = std::env::current_exe().unwrap();
    let launch = AcpLaunchConfig::new(&executable).resolve_command().unwrap();

    assert_eq!(launch.command, executable);
}

#[test]
fn executable_resolution_uses_platform_extensions_in_order() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let command_path = temp_dir.path().join("npx.cmd");
    std::fs::write(&command_path, "@echo off\r\n").unwrap();

    let resolved = find_executable_in_directories(
        "npx",
        [temp_dir.path().to_owned()],
        &[".exe".to_owned(), ".cmd".to_owned()],
    );

    assert_eq!(resolved, Some(command_path));
}

#[test]
fn executable_resolution_does_not_append_extensions_to_explicit_extension() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("agent.exe.cmd"), "@echo off\r\n").unwrap();

    let resolved = find_executable_in_directories(
        "agent.exe",
        [temp_dir.path().to_owned()],
        &[".cmd".to_owned()],
    );

    assert_eq!(resolved, None);
}

#[test]
fn windows_path_extensions_are_parsed_in_declared_order() {
    let extensions = windows_executable_extensions(Some(std::ffi::OsStr::new(".COM;.EXE; .CMD;")));

    assert_eq!(extensions, vec![".COM", ".EXE", ".CMD"]);
}

#[test]
fn windows_path_extensions_use_standard_fallback_when_missing_or_empty() {
    let expected = vec![".COM", ".EXE", ".BAT", ".CMD"];

    assert_eq!(windows_executable_extensions(None), expected);
    assert_eq!(
        windows_executable_extensions(Some(std::ffi::OsStr::new(" ; "))),
        expected
    );
}

#[test]
fn codex_path_sets_the_adapter_environment_variable() {
    let launch = AcpLaunchConfig::new("npx").codex_path(Path::new("/opt/codex"));

    assert_eq!(
        launch.env.get("CODEX_PATH").map(String::as_str),
        Some("/opt/codex")
    );
}

#[test]
fn initialization_timeout_is_configurable_and_bounded_by_default() {
    let default = AcpManagerConfig::new(AcpLaunchConfig::new("agent"));
    let custom = AcpManagerConfig::new(AcpLaunchConfig::new("agent"))
        .initialization_timeout(Duration::from_secs(2))
        .authentication_timeout(Duration::from_secs(3));

    assert_eq!(default.initialization_timeout, Duration::from_secs(30));
    assert_eq!(default.authentication_timeout, Duration::from_secs(5 * 60));
    assert_eq!(custom.initialization_timeout, Duration::from_secs(2));
    assert_eq!(custom.authentication_timeout, Duration::from_secs(3));
}
