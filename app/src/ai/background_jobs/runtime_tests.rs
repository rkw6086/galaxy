use super::*;

#[test]
fn partial_environment_overrides_preserve_inheritance_and_protected_guards() {
    let overrides = HashMap::from([
        ("KUBECONFIG".to_owned(), "/session/config".to_owned()),
        ("CI".to_owned(), "profile-value".to_owned()),
    ]);
    let environment = background_environment(Some(&overrides), false).unwrap();
    assert_eq!(
        environment.get("KUBECONFIG").map(String::as_str),
        Some("/session/config")
    );
    for (key, value) in NONINTERACTIVE_ENVIRONMENT {
        assert_eq!(environment.get(*key).map(String::as_str), Some(*value));
    }
    // These remain inherited from the execution host, not fabricated or cleared
    // when shell integration supplies an empty/partial map.
    assert!(!background_environment(Some(&HashMap::new()), false)
        .unwrap()
        .contains_key("HOME"));
    assert!(!environment.contains_key("PATH"));
    let windows =
        background_environment(Some(&HashMap::from([("ci".into(), "wrong".into())])), true)
            .unwrap();
    assert_eq!(windows.get("CI").map(String::as_str), Some("1"));
    assert!(!windows.contains_key("ci"));
}

#[test]
fn environment_override_names_cannot_inject_shell_syntax_or_private_setup() {
    for name in [
        "",
        "1BAD",
        "A;echo bad",
        "A=B",
        "A\0B",
        "_GALAXY_BACKGROUND_JOB_SETUP_KEY",
        "__galaxy_job_count",
        "argv",
    ] {
        let overrides = HashMap::from([(name.to_owned(), "test-value".to_owned())]);
        assert!(background_environment(Some(&overrides), false).is_err());
    }
}

#[cfg(unix)]
#[test]
fn fallback_shell_uses_only_a_recognized_absolute_host_shell() {
    assert_eq!(
        fallback_unix_shell(Some("/usr/bin/zsh".into())),
        (PathBuf::from("/usr/bin/zsh"), Some(ShellType::Zsh))
    );
    assert_eq!(
        fallback_unix_shell(Some("/usr/bin/fish".into())),
        (PathBuf::from("/usr/bin/fish"), Some(ShellType::Fish))
    );
    for value in [None, Some("bash".into()), Some("/bin/unknown-shell".into())] {
        assert_eq!(fallback_unix_shell(value), (PathBuf::from("/bin/sh"), None));
    }
}

#[test]
fn msys_bootstrap_paths_use_posix_drive_and_unc_forms() {
    assert_eq!(
        shell_path_argument(Path::new(r"C:\Program Files\Git\bin\bash.exe"), true).unwrap(),
        OsString::from("/c/Program Files/Git/bin/bash.exe")
    );
    assert_eq!(
        shell_path_argument(Path::new(r"\\?\C:\Users\name\project"), true).unwrap(),
        OsString::from("/c/Users/name/project")
    );
    assert_eq!(
        shell_path_argument(Path::new(r"\\?\UNC\server\share\project"), true).unwrap(),
        OsString::from("//server/share/project")
    );
}

#[cfg(unix)]
fn fixture_launch(
    shell: PathBuf,
    shell_type: ShellType,
    command: &str,
    cwd: &Path,
) -> BackgroundJobLaunch {
    BackgroundJobLaunch {
        conversation_id: crate::ai::agent::conversation::AIConversationId::new(),
        action_id: "profile-test-action".to_owned().into(),
        command: command.to_owned(),
        working_directory: cwd.to_path_buf(),
        shell: Some(ShellLaunchData::Executable {
            executable_path: shell,
            shell_type,
        }),
        environment: Some(HashMap::new()),
        host_id: None,
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn profile_bootstrap_keeps_command_noninteractive_and_restores_overrides_cwd_and_guards() {
    let root = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(root.path()).unwrap();
    let shell = cwd.join("fixture-bash");
    // Exercise actual bash initialization state and argv forwarding without
    // sourcing /etc/profile, the developer's HOME, or any real user rc files.
    write_executable(
        &shell,
        r#"#!/bin/sh
printf '%s\n' "$@" >> "$GALAXY_TEST_ARGV"
if [ "$1" = '-i' ] && [ "$2" = '-l' ] && [ "$3" = '+m' ] && [ "$4" = '-c' ]; then
    printf '%s' "$$" > "$GALAXY_TEST_PID"
    shift 4
    GALAXY_TEST_SETUP=$1
    export GALAXY_TEST_SETUP
    shift
    exec /bin/bash --noprofile --norc -i +m -c '. "$GALAXY_TEST_PROFILE"; builtin eval "$GALAXY_TEST_SETUP"' "$@"
fi
exec /bin/bash "$@"
"#,
    );
    let profile = cwd.join("fixture-profile");
    std::fs::write(
        &profile,
        r#"
case $- in *i*) ;; *) return ;; esac
if [ "$CI" = 1 ] || [ "$TERM" = dumb ]; then return; fi
export GALAXY_TEST_PROFILE_LOADED=loaded
export GALAXY_TEST_OVERRIDE=profile
export CI=profile
export SSH_ASKPASS=profile
export BASH_ENV="$GALAXY_TEST_BAD_STARTUP"
export PATH="$GALAXY_TEST_TOOLS:$PATH"
cd /
"#,
    )
    .unwrap();
    let tools = cwd.join("tools");
    std::fs::create_dir(&tools).unwrap();
    write_executable(
        &tools.join("galaxy-fixture-tool"),
        "#!/bin/sh\nprintf 'tool-found\\n'\n",
    );
    let bad_startup = cwd.join("unexpected-noninteractive-startup");
    std::fs::write(&bad_startup, "exit 97\n").unwrap();
    let text = r#"printf '%s\n' "$PWD" "$GALAXY_TEST_PROFILE_LOADED" "$GALAXY_TEST_OVERRIDE" "$CI" "${SSH_ASKPASS-unset}" "$$"
case $- in *i*) exit 99 ;; esac
if IFS= read -r ignored; then exit 98; fi
galaxy-fixture-tool
printf '%s\n' "a'b \"quoted\"" "line1
line2"
"#;
    let mut launch = fixture_launch(shell, ShellType::Bash, text, &cwd);
    let secret = "synthetic-session-value-not-for-argv";
    launch.environment = Some(HashMap::from([
        ("GALAXY_TEST_OVERRIDE".into(), secret.into()),
        ("CI".into(), "profile-startup".into()),
        ("TERM".into(), "profile-startup".into()),
    ]));
    let mut command = command_for_launch(&launch, &cwd).unwrap();
    let argv_log = cwd.join("argv");
    let pid_file = cwd.join("pid");
    command
        .current_dir(&cwd)
        .env("GALAXY_TEST_PROFILE", profile)
        .env("GALAXY_TEST_TOOLS", tools)
        .env("GALAXY_TEST_ARGV", &argv_log)
        .env("GALAXY_TEST_PID", &pid_file)
        .env("GALAXY_TEST_BAD_STARTUP", bad_startup)
        .env_remove("BASH_ENV");
    let output = command.output().await.unwrap();
    assert!(output.status.success());
    let original_pid = std::fs::read_to_string(pid_file).unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), format!("{}\nloaded\n{secret}\n1\nunset\n{original_pid}\ntool-found\na'b \"quoted\"\nline1\nline2\n", cwd.display()));
    assert!(!std::fs::read_to_string(argv_log).unwrap().contains(secret));
}

#[cfg(unix)]
#[tokio::test]
async fn powershell_setup_preserves_explicit_exit_native_failure_and_script_text() {
    let Some(pwsh) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join("pwsh"))
            .find(|path| path.is_file())
    }) else {
        // Optional host tool: the Unix bootstrap coverage does not require PowerShell.
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("fixture-pwsh");
    // Disable real profiles in the test host only. Production configuration
    // intentionally omits -NoProfile and uses the same constant setup script.
    write_executable(
        &shell,
        "#!/bin/sh\nexec \"$GALAXY_TEST_PWSH\" -NoProfile \"$@\"\n",
    );
    for (script, expected_code, expected_stdout) in [
        ("exit 17", 17, ""),
        ("& /bin/sh -c 'exit 9'", 1, ""),
        ("Write-Error 'failure'", 1, ""),
        (
            "Write-Error 'failure'; Write-Output 'success'",
            0,
            "success\n",
        ),
        (
            "param()\nWrite-Output 'quoted \"value\"'\n# trailing comment",
            0,
            "quoted \"value\"\n",
        ),
    ] {
        let launch = fixture_launch(shell.clone(), ShellType::PowerShell, script, root.path());
        let mut command = command_for_launch(&launch, root.path()).unwrap();
        command
            .current_dir(root.path())
            .env("GALAXY_TEST_PWSH", &pwsh);
        let output = command.output().await.unwrap();
        assert_eq!(output.status.code(), Some(expected_code));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected_stdout);
    }
}
