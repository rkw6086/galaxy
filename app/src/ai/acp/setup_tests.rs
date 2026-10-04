use super::*;

#[test]
fn incomplete_adapter_installation_is_not_launchable() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory
        .path()
        .join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, b"entrypoint").unwrap();
    assert!(!managed_adapter_ready(directory.path()));
    std::fs::write(
        directory.path().join(format!("ready-{CLAUDE_ACP_VERSION}")),
        b"ready",
    )
    .unwrap();
    assert!(managed_adapter_ready(directory.path()));
    std::fs::remove_file(script).unwrap();
    assert!(!managed_adapter_ready(directory.path()));
}

#[test]
fn official_package_binary_is_resolved_from_its_manifest() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("bin")).unwrap();
    std::fs::write(directory.path().join("bin/claude.exe"), b"binary").unwrap();
    std::fs::write(
        directory.path().join("package.json"),
        r#"{"bin":{"claude":"bin/claude.exe"}}"#,
    )
    .unwrap();
    assert_eq!(
        package_binary(directory.path()),
        Some(directory.path().join("bin/claude.exe"))
    );
}

#[test]
fn package_binary_cannot_escape_the_installation() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("package");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(directory.path().join("claude"), b"outside").unwrap();
    for binary in ["../claude", "/bin/sh", "missing"] {
        std::fs::write(
            package.join("package.json"),
            serde_json::json!({"bin": {"claude": binary}}).to_string(),
        )
        .unwrap();
        assert_eq!(package_binary(&package), None);
    }
}
