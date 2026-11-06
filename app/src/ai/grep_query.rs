//! Match-mode preparation shared by native and compatibility grep tool calls.

/// Keep regex mode unchanged; literal mode escapes only metacharacters shared
/// by git/grep extended regex, ripgrep, and PowerShell Select-String. This keeps
/// the persisted action and backend execution paths identical in both modes.
pub(crate) fn prepare_grep_queries(queries: Vec<String>, fixed_strings: bool) -> Vec<String> {
    if !fixed_strings {
        return queries;
    }
    queries
        .into_iter()
        .map(|query| {
            let mut escaped = String::with_capacity(query.len());
            for character in query.chars() {
                if matches!(
                    character,
                    '\\' | '.'
                        | '^'
                        | '$'
                        | '|'
                        | '?'
                        | '*'
                        | '+'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                ) {
                    escaped.push('\\');
                }
                escaped.push(character);
            }
            escaped
        })
        .collect()
}

#[cfg(test)]
#[path = "grep_query_tests.rs"]
mod tests;
