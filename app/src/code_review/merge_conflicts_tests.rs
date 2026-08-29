use super::{parse_conflicts, resolve_conflict, ConflictResolution};

#[test]
fn parses_multiple_conflicts_and_optional_base() {
    let text = "before\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> feature\nafter\n<<<<<<< HEAD\nleft\n=======\nright\n>>>>>>> feature\n";
    let conflicts = parse_conflicts(text);

    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].start_line, 2);
    assert_eq!(conflicts[0].ours, "ours\n");
    assert_eq!(conflicts[0].base.as_deref(), Some("base\n"));
    assert_eq!(conflicts[0].theirs, "theirs\n");
    assert_eq!(conflicts[1].base, None);
}

#[test]
fn ignores_incomplete_conflict_blocks() {
    let text = "before\n<<<<<<< HEAD\nours\n=======\ntheirs\n";
    assert!(parse_conflicts(text).is_empty());
}

#[test]
fn resolves_a_conflict_using_byte_ranges() {
    let text = "prefix\n<<<<<<< HEAD\n左\n=======\n右\n>>>>>>> feature\nsuffix\n";
    let (range, replacement) = resolve_conflict(text, 0, ConflictResolution::Theirs)
        .expect("complete conflict should resolve");

    assert_eq!(
        &text[range],
        "<<<<<<< HEAD\n左\n=======\n右\n>>>>>>> feature\n"
    );
    assert_eq!(replacement, "右\n");
}

#[test]
fn smart_merge_prefers_the_changed_side_when_other_side_matches_base() {
    let text = "<<<<<<< HEAD\nbase\n||||||| base\nbase\n=======\nchanged\n>>>>>>> feature\n";
    let (_, replacement) = resolve_conflict(text, 0, ConflictResolution::Smart).unwrap();
    assert_eq!(replacement, "changed\n");
}

#[test]
fn smart_merge_preserves_both_for_divergent_changes() {
    let text = "<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> feature\n";
    let (_, replacement) = resolve_conflict(text, 0, ConflictResolution::Smart).unwrap();
    assert_eq!(replacement, "ours\ntheirs\n");
}

#[test]
fn smart_merge_combines_non_overlapping_line_edits() {
    let text = "<<<<<<< HEAD\none\nours-two\nthree\n||||||| base\none\ntwo\nthree\n=======\none\ntwo\ntheirs-three\n>>>>>>> feature\n";
    let (_, replacement) = resolve_conflict(text, 0, ConflictResolution::Smart).unwrap();
    assert_eq!(replacement, "one\nours-two\ntheirs-three\n");
}
