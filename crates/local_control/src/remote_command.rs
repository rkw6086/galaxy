//! Conservative recognition of commands that can open an SSH-backed terminal.
//!
//! This is a narrow transport-boundary guard, not a network sandbox. Commands
//! that are otherwise authorized can still access the network through tools
//! other than the recognized SSH launch forms below.

use std::path::Path;

/// Returns whether a shell command appears capable of opening an SSH-backed
/// terminal, including common wrappers and compound command lists.
pub fn is_potential_remote_ssh_command(command: &str) -> bool {
    command_segments(command)
        .iter()
        .any(|segment| segment_starts_remote_ssh(segment, 0))
}

fn command_segments(command: &str) -> Vec<String> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut quote = Quote::None;
    let mut escaped = false;
    let mut current = String::new();
    let mut segments = Vec::new();
    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Quote::None => match character {
                '\\' => {
                    current.push(character);
                    escaped = true;
                }
                '\'' => {
                    current.push(character);
                    quote = Quote::Single;
                }
                '"' => {
                    current.push(character);
                    quote = Quote::Double;
                }
                ';' | '\n' | '|' | '&' | '(' | ')' | '`' => {
                    push_segment(&mut segments, &mut current);
                }
                _ => current.push(character),
            },
            Quote::Single => {
                current.push(character);
                if character == '\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => {
                current.push(character);
                match character {
                    '\\' => escaped = true,
                    '"' => quote = Quote::None,
                    // Backticks remain command substitutions inside double
                    // quotes, so inspect the enclosed command independently.
                    '`' => push_segment(&mut segments, &mut current),
                    _ => {}
                }
            }
        }
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    if !current.trim().is_empty() {
        segments.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn segment_starts_remote_ssh(segment: &str, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    let tokens = shell_words::split(segment).unwrap_or_else(|_| {
        segment
            .split_whitespace()
            .map(|token| token.trim_matches(['\'', '"', '`', '(', ')']).to_owned())
            .filter(|token| !token.is_empty())
            .collect()
    });
    command_tokens_start_remote_ssh(&tokens, depth)
}

fn command_tokens_start_remote_ssh(tokens: &[String], depth: usize) -> bool {
    let mut index = skip_assignments(tokens, 0);
    loop {
        let Some(executable) = tokens.get(index).map(|token| executable_name(token)) else {
            return false;
        };
        match executable {
            "command" => {
                index += 1;
                if tokens
                    .get(index)
                    .is_some_and(|option| matches!(option.as_str(), "-v" | "-V"))
                {
                    return false;
                }
                index = skip_flag_only_options(tokens, index);
            }
            "env" => {
                index = skip_env_prefix(tokens, index + 1);
            }
            "sudo" => {
                index = skip_sudo_prefix(tokens, index + 1);
            }
            "exec" | "nohup" | "setsid" | "time" => {
                index = skip_flag_only_options(tokens, index + 1);
            }
            "timeout" => {
                index = skip_timeout_prefix(tokens, index + 1);
            }
            "{" => {
                index += 1;
            }
            _ => break,
        }
        index = skip_assignments(tokens, index);
    }

    let executable = executable_name(&tokens[index]);
    if executable == "ssh" {
        return true;
    }
    if executable == "gcloud" {
        return tokens[index + 1..]
            .windows(2)
            .any(|pair| pair[0] == "compute" && pair[1] == "ssh");
    }
    if executable == "eb" {
        return tokens[index + 1..]
            .first()
            .is_some_and(|command| command == "ssh");
    }
    if executable == "doctl" {
        return tokens[index + 1..]
            .windows(2)
            .any(|pair| pair[0] == "compute" && pair[1] == "ssh");
    }
    if matches!(executable, "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish") {
        return shell_command_payload(tokens, index + 1)
            .is_some_and(|payload| segment_starts_remote_ssh(payload, depth + 1));
    }
    false
}

fn executable_name(token: &str) -> &str {
    Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token)
}

fn skip_assignments(tokens: &[String], mut index: usize) -> usize {
    while tokens.get(index).is_some_and(|token| is_assignment(token)) {
        index += 1;
    }
    index
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn skip_flag_only_options(tokens: &[String], mut index: usize) -> usize {
    while let Some(option) = tokens.get(index) {
        if option == "--" {
            return index + 1;
        }
        if !option.starts_with('-') || option == "-" {
            break;
        }
        index += 1;
    }
    index
}

fn skip_env_prefix(tokens: &[String], mut index: usize) -> usize {
    while let Some(option) = tokens.get(index) {
        if option == "--" {
            index += 1;
            break;
        }
        if matches!(
            option.as_str(),
            "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
        ) {
            index = (index + 2).min(tokens.len());
            continue;
        }
        if option.starts_with('-') && option != "-" {
            index += 1;
            continue;
        }
        break;
    }
    skip_assignments(tokens, index)
}

fn skip_sudo_prefix(tokens: &[String], mut index: usize) -> usize {
    while let Some(option) = tokens.get(index) {
        if option == "--" {
            return index + 1;
        }
        if matches!(
            option.as_str(),
            "-u" | "--user"
                | "-g"
                | "--group"
                | "-h"
                | "--host"
                | "-p"
                | "--prompt"
                | "-C"
                | "--chdir"
                | "-R"
                | "--chroot"
                | "-r"
                | "--role"
                | "-t"
                | "--type"
        ) {
            index = (index + 2).min(tokens.len());
            continue;
        }
        if option.starts_with('-') && option != "-" {
            index += 1;
            continue;
        }
        break;
    }
    index
}

fn skip_timeout_prefix(tokens: &[String], mut index: usize) -> usize {
    while let Some(option) = tokens.get(index) {
        if option == "--" {
            index += 1;
            break;
        }
        if matches!(option.as_str(), "-k" | "--kill-after" | "-s" | "--signal") {
            index = (index + 2).min(tokens.len());
            continue;
        }
        if option.starts_with('-') && option != "-" {
            index += 1;
            continue;
        }
        break;
    }
    // The first non-option is timeout's duration, not its child executable.
    (index + usize::from(index < tokens.len())).min(tokens.len())
}

fn shell_command_payload(tokens: &[String], mut index: usize) -> Option<&str> {
    while let Some(option) = tokens.get(index) {
        if option == "--" {
            index += 1;
            continue;
        }
        if option.starts_with('-') && option.contains('c') {
            return tokens.get(index + 1).map(String::as_str);
        }
        if !option.starts_with('-') {
            return None;
        }
        index += 1;
    }
    None
}

#[cfg(test)]
#[path = "remote_command_tests.rs"]
mod tests;
