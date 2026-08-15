use super::is_interactive_remote_command;
#[test]
fn raw_interactive_ssh_is_treated_as_remote_for_acp() {
    for command in [
        "ssh user@example.com",
        "command ssh -p 2222 user@example.com",
        "   /usr/bin/ssh user@example.com",
        "GALAXY_TEST=1 ssh user@example.com",
        "env GALAXY_TEST=1 ssh user@example.com",
        "/usr/bin/env -- GALAXY_TEST=1 /usr/bin/ssh user@example.com",
        "sudo -u root ssh user@example.com",
        "cd /tmp && ssh user@example.com",
        "bash -lc 'ssh user@example.com'",
        "gcloud compute ssh --zone us-central1-a instance",
        "ssh user@example.com uname -a",
        "ssh -T git@example.com",
    ] {
        assert!(is_interactive_remote_command(command), "{command}");
    }
    for command in [
        "cargo test",
        "echo /usr/bin/ssh user@example.com",
        "GALAXY_TEST=/usr/bin/ssh cargo test",
        "env GALAXY_TEST=1 cargo test",
        "/usr/bin/ssh-add user@example.com",
        "bash -lc 'echo ssh user@example.com'",
    ] {
        assert!(!is_interactive_remote_command(command), "{command}");
    }
}
