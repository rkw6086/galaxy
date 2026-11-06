use super::*;
use crate::terminal::model::secrets::regexes::FIREBASE_AUTH_DOMAIN;
use crate::terminal::shell::ShellType;

#[test]
fn test_create_redacted_grep_error_event() {
    crate::terminal::model::set_user_and_enterprise_secret_regexes(
        [&regex::Regex::new(FIREBASE_AUTH_DOMAIN).expect("Should be able to construct regex")],
        std::iter::empty(), // No enterprise secrets
    );

    // Create input with a known secret pattern (Firebase domain)
    let queries = vec![
        "normal query".to_string(),
        "query with warp-server-staging.firebaseapp.com secret".to_string(),
    ];
    let path = "path/to/file/with/warp-server-staging.firebaseapp.com/secret".to_string();
    let shell_type = Some(ShellType::Bash);
    let working_directory = Some("/users/test/warp-server-staging.firebaseapp.com".to_string());
    let absolute_path =
        "/absolute/path/with/warp-server-staging.firebaseapp.com/secret".to_string();
    let error = GrepError::new("Error message".to_string())
        .with_command("grep warp-server-staging.firebaseapp.com".to_string())
        .with_output("Output with warp-server-staging.firebaseapp.com".to_string());

    // Call the function with the test inputs
    let event = create_redacted_grep_error_event(
        true,
        None,
        queries.clone(),
        path.clone(),
        shell_type,
        working_directory.clone(),
        absolute_path.clone(),
        error,
    );

    // Verify the telemetry event has redacted secrets
    if let TelemetryEvent::GrepToolFailed {
        queries: Some(redacted_queries),
        path: Some(redacted_path),
        shell_type: _,
        working_directory: Some(redacted_working_directory),
        absolute_path: Some(redacted_absolute_path),
        command: Some(redacted_command),
        output: Some(redacted_output),
        error: _,
        server_output_id: _,
    } = event
    {
        // Verify secrets are redacted from all relevant fields
        assert_eq!(redacted_queries.len(), 2);
        assert_eq!(redacted_queries[0], "normal query");
        assert!(!redacted_queries[1].contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_queries[1].contains("*****"));

        assert!(!redacted_path.contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_path.contains("*****"));

        assert!(!redacted_working_directory.contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_working_directory.contains("*****"));

        assert!(!redacted_absolute_path.contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_absolute_path.contains("*****"));

        assert!(!redacted_command.contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_command.contains("*****"));

        assert!(!redacted_output.contains("warp-server-staging.firebaseapp.com"));
        assert!(redacted_output.contains("*****"));
    } else {
        panic!("Expected GrepToolFailed event");
    }
}

#[test]
fn build_git_grep_command_single_quotes_shell_substitution() {
    let queries = vec!["$(touch /tmp/warp-poc); `id`".to_string()];

    let command = build_git_grep_command(&queries, "/tmp/repo path", ShellType::Bash);

    assert_eq!(
        command,
        "git --no-pager grep --color=never --untracked -nIE -m 50 -e '$(touch /tmp/warp-poc); `id`' '/tmp/repo path'"
    );
}

#[test]
fn build_grep_command_escapes_single_quotes() {
    let queries = vec!["owner's code".to_string()];

    let command = build_grep_command(&queries, "/tmp/repo", ShellType::Bash);

    assert_eq!(
        command,
        r#"grep --color=never -nrIHE --devices=skip -m 50 -e 'owner'"'"'s code' '/tmp/repo'"#
    );
}

#[test]
fn build_select_string_command_single_quotes_powershell_substitution() {
    let queries = vec![r#"$(New-Item C:\pwn); 'literal'"#.to_string()];

    let command = build_select_string_command(&queries, r#"C:\repo path"#);

    assert_eq!(
        command,
        r#"Get-ChildItem -Path 'C:\repo path' -Recurse -File | Select-String -NoEmphasis -CaseSensitive -Pattern '$(New-Item C:\pwn); ''literal''' | Select-Object -First 1001"#
    );
}

#[test]
fn parse_grep_output_reports_when_results_reach_the_per_file_limit() {
    let output = (1..=GREP_MAX_MATCHES_PER_FILE)
        .map(|line| format!("src/lib.rs:{line}:match"))
        .collect::<Vec<_>>()
        .join("\n");

    let (matched_files, truncated) = parse_grep_output(&output, None, None).unwrap();

    assert_eq!(matched_files.len(), 1);
    assert_eq!(
        matched_files[0].matched_lines.len(),
        GREP_MAX_MATCHES_PER_FILE
    );
    assert!(truncated);
}

#[test]
fn backend_regex_syntax_errors_explain_literal_and_regex_recovery() {
    for output in [
        "fatal: -e option, 'variantResolutions: []': brackets ([ ]) not balanced",
        "fatal: -e option, 'indexAsset(context': parentheses not balanced",
        "fatal: command line, 'variantResolutions: []': Unmatched [, [^, [:, [., or [=",
        r"fatal: command line, 'indexAsset(context': Unmatched ( or \(",
        "regex parse error: unclosed group",
        "Select-String: Invalid pattern 'indexAsset(context': Not enough )'s.",
    ] {
        let error = GrepError::new_for_non_zero_exit_code().with_output(output.to_owned());
        let message = error.error_for_conversation();
        assert!(message.contains(output));
        assert!(message.contains("fixed_strings: true"));
        assert!(message.contains("correct or escape"));
        assert_eq!(error.error_message(), NON_ZERO_EXIT_CODE_ERROR);
    }
}

#[test]
fn ripgrep_regex_errors_also_include_literal_mode_guidance() {
    let error =
        GrepError::new("Ripgrep search failed: regex parse error: unclosed group".to_owned());
    assert!(error
        .error_for_conversation()
        .contains("fixed_strings: true"));
}

#[test]
fn unrelated_grep_failures_keep_the_original_diagnostic() {
    let output = "fatal: cannot access '/project': Permission denied";
    let error = GrepError::new_for_non_zero_exit_code().with_output(output.to_owned());
    assert_eq!(
        error.error_for_conversation(),
        format!("{NON_ZERO_EXIT_CODE_ERROR}, output:\n{output}")
    );
    assert_eq!(
        GrepError::new("Grep operation timed out".to_owned()).error_for_conversation(),
        "Grep operation timed out"
    );
}
