use std::collections::HashMap;

use ai::agent::action_result::AnyFileContent;
use ai::agent::FileLocations;
use ai::diff_validation::DiffType;

use super::{
    updated_file_contexts_from_editor_buffers, AIAgentActionId, DiffSessionType, FileDiff,
    PendingAppliedDiffs,
};

#[test]
fn applied_diffs_survive_until_delayed_view_registration() {
    let action_id = AIAgentActionId::from("file-edit".to_string());
    let mut pending = PendingAppliedDiffs::default();
    pending.buffer(
        action_id.clone(),
        vec![FileDiff::new(
            "before".to_string(),
            "/workspace/src/main.rs".to_string(),
            DiffType::update(vec![], None),
        )],
        DiffSessionType::Local,
    );

    let (diffs, session_type) = pending
        .take(&action_id)
        .expect("buffered diffs should remain available for registration");
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].base.content, "before");
    assert_eq!(diffs[0].base.file_path, "/workspace/src/main.rs");
    assert!(matches!(session_type, DiffSessionType::Local));
    assert!(pending.take(&action_id).is_none());
}

#[test]
fn updated_file_contexts_from_editor_buffers_returns_changed_lines_with_context() {
    let updated_files = vec![(
        FileLocations {
            name: "src/main.rs".to_string(),
            lines: std::iter::once(12..13).collect(),
        },
        true,
    )];
    let content = (1..=30)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let content_map = HashMap::from([("src/main.rs".to_string(), content)]);

    let contexts = updated_file_contexts_from_editor_buffers(&updated_files, &content_map);

    assert_eq!(contexts.len(), 1);
    assert!(contexts[0].was_edited_by_user);
    assert_eq!(contexts[0].file_context.file_name, "src/main.rs");
    assert_eq!(contexts[0].file_context.line_range, Some(2..23));
    assert_eq!(contexts[0].file_context.line_count, 30);
    assert_eq!(
        contexts[0].file_context.content,
        AnyFileContent::StringContent(
            (2..=22)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    );
}

#[test]
fn updated_file_contexts_from_editor_buffers_preserves_full_file_when_no_ranges() {
    let updated_files = vec![(
        FileLocations {
            name: "src/main.rs".to_string(),
            lines: vec![],
        },
        false,
    )];
    let content = "line 1\nline 2\n".to_string();
    let content_map = HashMap::from([("src/main.rs".to_string(), content.clone())]);

    let contexts = updated_file_contexts_from_editor_buffers(&updated_files, &content_map);

    assert_eq!(contexts.len(), 1);
    assert!(!contexts[0].was_edited_by_user);
    assert_eq!(contexts[0].file_context.line_range, None);
    assert_eq!(contexts[0].file_context.line_count, 2);
    assert_eq!(
        contexts[0].file_context.content,
        AnyFileContent::StringContent(content)
    );
}
