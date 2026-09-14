//! Bounded discovery excerpts. Explicit `read_files` calls provide full follow-up context.
use std::collections::HashSet;

pub(super) use ai::agent::action_result::MAX_SEARCH_CODEBASE_FILES as MAX_SEARCH_FILES;

use crate::ai::agent::{AnyFileContent, FileContext, SearchCodebaseResult};
pub(super) const MAX_SEARCH_SCAN_BYTES: usize = 256_000;
const MAX_EXCERPT_BYTES: usize = 4_000;
const MAX_EXCERPT_LINES: usize = 40;

pub(super) fn bound_search_result(
    result: SearchCodebaseResult,
    query: &str,
) -> SearchCodebaseResult {
    let SearchCodebaseResult::Success { files } = result else {
        return result;
    };
    let terms: Vec<_> = query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| term.len() >= 3)
        .map(str::to_lowercase)
        .filter(|term| {
            !matches!(
                term.as_str(),
                "the"
                    | "and"
                    | "for"
                    | "with"
                    | "from"
                    | "that"
                    | "this"
                    | "find"
                    | "show"
                    | "all"
                    | "code"
                    | "files"
                    | "where"
                    | "what"
                    | "when"
                    | "which"
                    | "into"
            )
        })
        .take(32)
        .collect();
    let mut seen = HashSet::new();
    let files = files
        .into_iter()
        .filter(|file| seen.insert(file.file_name.clone()))
        .filter_map(|file| excerpt(file, &terms))
        .take(MAX_SEARCH_FILES)
        .collect();
    SearchCodebaseResult::Success { files }
}

fn excerpt(mut file: FileContext, terms: &[String]) -> Option<FileContext> {
    let AnyFileContent::StringContent(content) = &file.content else {
        return None;
    };
    // Also bound remote payloads before scoring. Never split a UTF-8 code point.
    let mut scan_end = content.len().min(MAX_SEARCH_SCAN_BYTES);
    while !content.is_char_boundary(scan_end) {
        scan_end -= 1;
    }
    let lines: Vec<_> = content[..scan_end].split_inclusive('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let mut best_line = 0;
    let mut best_score = 0;
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        let score: usize = terms
            .iter()
            .filter(|term| lower.contains(term.as_str()))
            .map(String::len)
            .sum();
        if score > best_score {
            best_score = score;
            best_line = index;
        }
    }
    let mut start = best_line;
    let mut preceding_bytes = 0;
    while start > best_line.saturating_sub(5) {
        let length = lines[start - 1].len();
        if preceding_bytes + length > MAX_EXCERPT_BYTES / 3 {
            break;
        }
        preceding_bytes += length;
        start -= 1;
    }
    let mut text = String::new();
    let mut line_count: usize = 0;
    for line in lines.iter().skip(start).take(MAX_EXCERPT_LINES) {
        let remaining = MAX_EXCERPT_BYTES - text.len();
        if remaining == 0 {
            break;
        }
        let mut end = line.len().min(remaining);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            break;
        }
        text.push_str(&line[..end]);
        line_count += 1;
        if end < line.len() {
            break;
        }
    }
    let first_line = file.line_range.as_ref().map_or(1, |range| range.start) + start;
    file.line_range = Some(first_line..first_line + line_count.saturating_sub(1));
    file.content = AnyFileContent::StringContent(text);
    Some(file)
}

#[cfg(test)]
#[path = "search_codebase_context_tests.rs"]
mod tests;
