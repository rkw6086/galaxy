use super::*;
use crate::ai::agent::{AnyFileContent, FileContext};

fn remote_file() -> remote_server::proto::FileContextProto {
    remote_server::proto::FileContextProto {
        file_name: "/project/present.rs".to_owned(),
        content: Some(
            remote_server::proto::file_context_proto::Content::TextContent(
                "returned file content".to_owned(),
            ),
        ),
        line_range_start: Some(3),
        line_range_end: Some(5),
        last_modified_epoch_millis: Some(42),
        line_count: 20,
    }
}

#[test]
fn partial_remote_reads_report_failed_paths_instead_of_claiming_success() {
    let result = remote_read_files_result(remote_server::proto::ReadFileContextResponse {
        file_contexts: vec![remote_file()],
        failed_files: vec![remote_server::proto::FailedFileRead {
            path: "/project/restricted.rs".to_owned(),
            error: Some(remote_server::proto::FileOperationError {
                message: "Permission denied".to_owned(),
            }),
        }],
    });
    let ReadFilesResult::Error(message) = result else {
        panic!("A partial remote read must not claim full success");
    };
    assert!(message.contains("/project/restricted.rs: Permission denied"));
    assert!(message.contains("No file contents were returned for this batch"));
    assert!(!message.contains("returned file content"));
    assert!(message.contains("confirm exact paths"));
}

#[test]
fn remote_file_context_without_content_is_an_explicit_failure() {
    let mut missing = remote_file();
    missing.file_name = "/project/no-content.rs".to_owned();
    missing.content = None;
    let result = remote_read_files_result(remote_server::proto::ReadFileContextResponse {
        file_contexts: vec![remote_file(), missing.clone()],
        failed_files: vec![],
    });
    let ReadFilesResult::Error(message) = result else {
        panic!("A remote context without content must not disappear into Success");
    };
    assert!(message.contains("/project/no-content.rs: remote host returned no file content"));
    assert!(!message.contains("returned file content"));

    let result = remote_read_files_result(remote_server::proto::ReadFileContextResponse {
        file_contexts: vec![missing],
        failed_files: vec![remote_server::proto::FailedFileRead {
            path: "/project/restricted.rs".to_owned(),
            error: Some(remote_server::proto::FileOperationError {
                message: "Permission denied".to_owned(),
            }),
        }],
    });
    let ReadFilesResult::Error(message) = result else {
        panic!("Both remote failure categories must remain visible");
    };
    assert!(message.contains("/project/no-content.rs: remote host returned no file content"));
    assert!(message.contains("/project/restricted.rs: Permission denied"));
}

#[test]
fn successful_remote_reads_preserve_content_ranges_and_file_metadata() {
    let result = remote_read_files_result(remote_server::proto::ReadFileContextResponse {
        file_contexts: vec![remote_file()],
        failed_files: vec![],
    });
    let ReadFilesResult::Success { files } = result else {
        panic!("A complete remote read must succeed");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name, "/project/present.rs");
    assert_eq!(
        files[0].content,
        AnyFileContent::StringContent("returned file content".to_owned())
    );
    assert_eq!(files[0].line_range, Some(3..5));
    assert_eq!(files[0].line_count, 20);
    assert_eq!(
        files[0].last_modified,
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(42))
    );
}

#[test]
fn local_failures_do_not_assert_that_oversized_or_unreadable_files_are_missing() {
    let result = local_read_files_result(super::super::ReadFileContextResult {
        file_contexts: vec![],
        missing_files: vec!["/project/large.bin".to_owned()],
    });
    let ReadFilesResult::Error(message) = result else {
        panic!("An unread file must remain an error");
    };
    assert!(message.contains("/project/large.bin"));
    assert!(message.contains("exceeds the read limit"));
    assert!(message.contains("smaller line range or fewer files"));
    assert!(!message.contains("do not exist"));
}

#[test]
fn successful_local_reads_preserve_their_original_content() {
    let file = FileContext {
        file_name: "/project/present.rs".to_owned(),
        content: AnyFileContent::StringContent("unchanged".to_owned()),
        line_range: None,
        last_modified: None,
        line_count: 1,
    };
    assert_eq!(
        local_read_files_result(super::super::ReadFileContextResult {
            file_contexts: vec![file.clone()],
            missing_files: vec![],
        }),
        ReadFilesResult::Success { files: vec![file] },
    );
}

#[test]
fn failure_details_bound_unicode_paths_reasons_and_the_number_of_reported_files() {
    let long_field = "界".repeat(10_000);
    let result = read_files_failure(
        (0..MAX_REPORTED_READ_FAILURES + 3)
            .map(|_| (long_field.as_str(), Some(long_field.as_str()))),
    );
    let ReadFilesResult::Error(message) = result else {
        panic!("Failed paths must produce an error");
    };
    assert_eq!(message.matches("\n- ").count(), MAX_REPORTED_READ_FAILURES);
    assert!(message.contains("3 additional failed paths omitted"));
    assert!(message.contains('…'));
    assert!(
        message.chars().count()
            < MAX_REPORTED_READ_FAILURES * MAX_READ_FAILURE_FIELD_CHARS * 2 + 1_000
    );
}
