//! Deterministic input detection using the shell's existing completion inventory and history.
//! Never executes the input or invokes a model.
use galaxy_completer::completer::CompletionContext;
use galaxy_completer::parsers::simple::top_level_command;
use galaxy_completer::ParsedTokensSnapshot;
use galaxy_core::command::ExitCode;
use galaxy_util::path::EscapeChar;
use input_classifier::InputType;

/// Unknown outcomes and interrupted/failed input are not evidence that a command exists.
/// Do not use `was_successful`: it also accepts interruptions for terminal UI purposes.
pub(crate) fn is_verified_history_command(exit_code: Option<ExitCode>) -> bool {
    exit_code.is_some_and(|code| code.value() == 0)
}

pub(crate) fn detect_input_type(
    input: &ParsedTokensSnapshot,
    context: &impl CompletionContext,
    history: &[String],
    current: InputType,
) -> InputType {
    let escape = context.escape_char();
    let Some(command) = top_level_command(&input.buffer_text, escape) else {
        return current;
    };
    let matches = |candidate: &str| {
        if matches!(escape, EscapeChar::Backtick) {
            candidate.eq_ignore_ascii_case(&command)
        } else {
            candidate == command
        }
    };
    let known = context.top_level_commands().any(matches)
        || context.aliases().any(|(name, _)| matches(name))
        || context
            .functions()
            .is_some_and(|names| names.iter().any(|name| matches(name)))
        || context
            .builtins()
            .is_some_and(|names| names.iter().any(|name| matches(name)))
        || context
            .abbreviations()
            .is_some_and(|names| names.keys().any(|name| matches(name)))
        || history
            .iter()
            .filter_map(|entry| top_level_command(entry, escape))
            .any(|name| matches(&name));
    // Explicit paths and shell constructs indicate command intent even while incomplete.
    if known
        || command.starts_with(['/', '~'])
        || command.starts_with("./")
        || command.starts_with("../")
        || matches!(
            command.as_str(),
            "if" | "for" | "while" | "until" | "case" | "function" | "begin" | "{"
        )
    {
        return InputType::Shell;
    }
    if !input.buffer_text.contains(char::is_whitespace)
        && context
            .top_level_commands()
            .chain(context.aliases().map(|(name, _)| name))
            .any(|name| name.starts_with(&command))
    {
        return current;
    }
    InputType::AI
}

#[cfg(test)]
#[path = "input_classifier_tests.rs"]
mod tests;
