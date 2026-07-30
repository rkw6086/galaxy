use super::is_potential_remote_ssh_command;

#[test]
fn recognizes_direct_wrapped_and_compound_ssh_launches() {
    for command in [
        "ssh user@example.com",
        "/usr/bin/ssh -T git@example.com",
        "command ssh user@example.com",
        "env GALAXY_TEST=1 ssh user@example.com",
        "sudo ssh user@example.com",
        "sudo -u root /usr/bin/ssh user@example.com",
        "cd /tmp && ssh user@example.com",
        "printf done; sudo -n ssh user@example.com",
        "bash -lc 'ssh user@example.com'",
        "timeout 10 ssh user@example.com",
        "gcloud compute ssh --zone us-central1-a instance",
        "eb ssh environment",
        "doctl compute ssh droplet-action",
    ] {
        assert!(is_potential_remote_ssh_command(command), "{command}");
    }
}

#[test]
fn ignores_ssh_text_and_non_session_utilities() {
    for command in [
        "",
        "cargo test",
        "echo ssh user@example.com",
        "printf '%s' 'ssh user@example.com'",
        "GALAXY_TEST=ssh cargo test",
        "ssh-add ~/.ssh/id_ed25519",
        "command -v ssh",
        "bash -lc 'echo ssh user@example.com'",
    ] {
        assert!(!is_potential_remote_ssh_command(command), "{command}");
    }
}
