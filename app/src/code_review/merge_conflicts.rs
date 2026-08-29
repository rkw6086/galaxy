//! Parsing and resolution helpers for Git's conflict-marker format.
//!
//! This is deliberately separate from unified-diff parsing. A conflicted
//! working tree contains the current file with marker blocks, while a unified
//! diff describes two versions of a file. Keeping the representations separate
//! makes the merge actions safe to apply to the live editor buffer.

use std::ops::Range;

use similar::{DiffOp, TextDiff};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConflictBlock {
    /// Byte range in the original text covering the complete marker block.
    pub(crate) byte_range: Range<usize>,
    /// One-indexed line containing the opening marker.
    pub(crate) start_line: usize,
    pub(crate) ours: String,
    pub(crate) base: Option<String>,
    pub(crate) theirs: String,
}

impl ConflictBlock {
    /// Apply Git's common three-way merge rules. When both sides changed the
    /// same base differently, preserving both sides is safer than silently
    /// choosing one.
    pub(crate) fn smart_merge(&self) -> String {
        let Some(base) = self.base.as_deref() else {
            return if self.ours == self.theirs {
                self.ours.clone()
            } else {
                merge_both(&self.ours, &self.theirs)
            };
        };

        if self.ours == self.theirs {
            return self.ours.clone();
        }
        if self.ours == base {
            return self.theirs.clone();
        }
        if self.theirs == base {
            return self.ours.clone();
        }

        three_way_merge(base, &self.ours, &self.theirs)
            .unwrap_or_else(|| merge_both(&self.ours, &self.theirs))
    }

    pub(crate) fn resolve(&self, resolution: ConflictResolution) -> String {
        match resolution {
            ConflictResolution::Ours => self.ours.clone(),
            ConflictResolution::Theirs => self.theirs.clone(),
            ConflictResolution::Both => merge_both(&self.ours, &self.theirs),
            ConflictResolution::Smart => self.smart_merge(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConflictResolution {
    Ours,
    Theirs,
    Both,
    Smart,
}

/// Return all complete conflict blocks in `text`, preserving each side's
/// original line endings. Malformed or incomplete marker sequences are ignored
/// so a partially written file is never destructively rewritten.
pub(crate) fn parse_conflicts(text: &str) -> Vec<ConflictBlock> {
    let lines = text_lines(text);
    let mut conflicts = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let (line_start, _, line) = lines[index];
        if !line_content(line).starts_with("<<<<<<<") {
            index += 1;
            continue;
        }

        let start_line = index + 1;
        let mut cursor = index + 1;
        let mut ours = String::new();
        while cursor < lines.len() {
            let content = line_content(lines[cursor].2);
            if content.starts_with("|||||||") || content == "=======" {
                break;
            }
            if content.starts_with("<<<<<<<") || content.starts_with(">>>>>>>") {
                break;
            }
            ours.push_str(lines[cursor].2);
            cursor += 1;
        }

        let mut base = None;
        if cursor < lines.len() && line_content(lines[cursor].2).starts_with("|||||||") {
            cursor += 1;
            let mut base_content = String::new();
            while cursor < lines.len() {
                let content = line_content(lines[cursor].2);
                if content == "=======" || content.starts_with(">>>>>>>") {
                    break;
                }
                base_content.push_str(lines[cursor].2);
                cursor += 1;
            }
            base = Some(base_content);
        }

        if cursor >= lines.len() || line_content(lines[cursor].2) != "=======" {
            index += 1;
            continue;
        }

        cursor += 1;
        let mut theirs = String::new();
        while cursor < lines.len() {
            let content = line_content(lines[cursor].2);
            if content.starts_with(">>>>>>>") {
                break;
            }
            if content.starts_with("<<<<<<<") {
                break;
            }
            theirs.push_str(lines[cursor].2);
            cursor += 1;
        }

        if cursor >= lines.len() || !line_content(lines[cursor].2).starts_with(">>>>>>>") {
            index += 1;
            continue;
        }

        conflicts.push(ConflictBlock {
            byte_range: line_start..lines[cursor].1,
            start_line,
            ours,
            base,
            theirs,
        });
        index = cursor + 1;
    }

    conflicts
}

/// Resolve one block in `text`, returning the complete replacement text.
/// Callers should reparse after every edit because previous resolutions change
/// subsequent offsets.
pub(crate) fn resolve_conflict(
    text: &str,
    conflict_index: usize,
    resolution: ConflictResolution,
) -> Option<(Range<usize>, String)> {
    let conflict = parse_conflicts(text).into_iter().nth(conflict_index)?;
    let replacement = conflict.resolve(resolution);
    Some((conflict.byte_range, replacement))
}

#[derive(Debug)]
struct LineEdit {
    base_range: Range<usize>,
    replacement: String,
}

fn three_way_merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let base_lines = split_lines(base);
    let ours_lines = split_lines(ours);
    let theirs_lines = split_lines(theirs);
    let ours_edits = line_edits(&base_lines, &ours_lines);
    let theirs_edits = line_edits(&base_lines, &theirs_lines);

    let mut merged = String::new();
    let mut base_index = 0;
    let mut ours_index = 0;
    let mut theirs_index = 0;

    while ours_index < ours_edits.len() || theirs_index < theirs_edits.len() {
        let ours_edit = ours_edits.get(ours_index);
        let theirs_edit = theirs_edits.get(theirs_index);
        let next_start = match (ours_edit, theirs_edit) {
            (Some(ours), Some(theirs)) => ours.base_range.start.min(theirs.base_range.start),
            (Some(ours), None) => ours.base_range.start,
            (None, Some(theirs)) => theirs.base_range.start,
            (None, None) => break,
        };

        merged.push_str(&base_lines[base_index..next_start].concat());

        match (ours_edit, theirs_edit) {
            (Some(ours), Some(theirs)) if ranges_overlap(&ours.base_range, &theirs.base_range) => {
                if ours.base_range == theirs.base_range && ours.replacement == theirs.replacement {
                    merged.push_str(&ours.replacement);
                    base_index = ours.base_range.end;
                    ours_index += 1;
                    theirs_index += 1;
                } else {
                    return None;
                }
            }
            (Some(ours), Some(theirs)) if ours.base_range.start <= theirs.base_range.start => {
                merged.push_str(&ours.replacement);
                base_index = ours.base_range.end;
                ours_index += 1;
            }
            (Some(_), Some(theirs)) => {
                merged.push_str(&theirs.replacement);
                base_index = theirs.base_range.end;
                theirs_index += 1;
            }
            (Some(ours), None) => {
                merged.push_str(&ours.replacement);
                base_index = ours.base_range.end;
                ours_index += 1;
            }
            (None, Some(theirs)) => {
                merged.push_str(&theirs.replacement);
                base_index = theirs.base_range.end;
                theirs_index += 1;
            }
            (None, None) => unreachable!(),
        }
    }

    merged.push_str(&base_lines[base_index..].concat());
    Some(merged)
}

fn line_edits(base: &[&str], changed: &[&str]) -> Vec<LineEdit> {
    TextDiff::configure()
        .algorithm(similar::Algorithm::Patience)
        .diff_lines(&base.concat(), &changed.concat())
        .ops()
        .iter()
        .filter_map(|op| match op {
            DiffOp::Equal { .. } => None,
            DiffOp::Delete {
                old_index, old_len, ..
            } => Some(LineEdit {
                base_range: *old_index..*old_index + *old_len,
                replacement: String::new(),
            }),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => Some(LineEdit {
                base_range: *old_index..*old_index,
                replacement: changed[*new_index..*new_index + *new_len].concat(),
            }),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => Some(LineEdit {
                base_range: *old_index..*old_index + *old_len,
                replacement: changed[*new_index..*new_index + *new_len].concat(),
            }),
        })
        .collect()
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    if left.start == left.end || right.start == right.end {
        left.start == right.start
    } else {
        left.start < right.end && right.start < left.end
    }
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<_> = text.split_inclusive('\n').collect();
    if lines.last().is_some_and(|line| !line.ends_with('\n')) {
        return lines;
    }
    if text.is_empty() {
        lines.clear();
    }
    lines
}

fn merge_both(ours: &str, theirs: &str) -> String {
    if ours.is_empty() {
        return theirs.to_string();
    }
    if theirs.is_empty() {
        return ours.to_string();
    }
    if ours.ends_with('\n') || theirs.starts_with('\n') {
        format!("{ours}{theirs}")
    } else {
        format!("{ours}\n{theirs}")
    }
}

fn line_content(line: &str) -> &str {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .strip_suffix('\r')
        .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line))
}

fn text_lines(text: &str) -> Vec<(usize, usize, &str)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        lines.push((start, end, line));
        start = end;
    }
    if start < text.len() || text.is_empty() {
        lines.push((start, text.len(), &text[start..]));
    }
    lines
}

#[cfg(test)]
#[path = "merge_conflicts_tests.rs"]
mod tests;
