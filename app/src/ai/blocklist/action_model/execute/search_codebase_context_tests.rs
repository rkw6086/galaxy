use super::*;

fn file(name: &str, text: String) -> FileContext {
    FileContext::new(
        name.to_string(),
        AnyFileContent::StringContent(text),
        None,
        None,
    )
}

#[test]
fn search_returns_bounded_unique_files_with_valid_utf8() {
    let files = (0..20)
        .flat_map(|index| {
            let file = file(&format!("{index}.rs"), "€".repeat(20_000));
            [file.clone(), file]
        })
        .collect();
    let SearchCodebaseResult::Success { files } =
        bound_search_result(SearchCodebaseResult::Success { files }, "find models")
    else {
        panic!("expected success");
    };
    assert_eq!(files.len(), MAX_SEARCH_FILES);
    let mut bytes = 0;
    for file in files {
        let AnyFileContent::StringContent(text) = file.content else {
            panic!("expected text");
        };
        assert!(text.len() <= MAX_EXCERPT_BYTES);
        assert_eq!(text.len() % 3, 0);
        bytes += text.len();
    }
    assert!(bytes <= MAX_SEARCH_FILES * MAX_EXCERPT_BYTES);
}

#[test]
fn excerpt_finds_a_later_match_and_preserves_source_line_numbers() {
    let text = format!(
        "{}fn construct_chat_model() {{}}\n{}",
        "// unrelated\n".repeat(200),
        "// trailing\n".repeat(100)
    );
    let mut input = file("models.rs", text);
    input.line_range = Some(101..401);
    let output = excerpt(input, &["construct_chat_model".into()]).unwrap();
    assert_eq!(output.line_range, Some(296..335));
    let AnyFileContent::StringContent(text) = output.content else {
        panic!("expected text");
    };
    assert!(text.contains("construct_chat_model"));
    assert_eq!(text.lines().count(), MAX_EXCERPT_LINES);
}

#[test]
fn long_preceding_line_does_not_hide_the_relevant_match() {
    let input = file(
        "models.rs",
        format!("{}\nfn target_model() {{}}\n", "x".repeat(10_000)),
    );
    let output = excerpt(input, &["target_model".into()]).unwrap();
    assert_eq!(output.line_range, Some(2..2));
    let AnyFileContent::StringContent(text) = output.content else {
        panic!("expected text");
    };
    assert!(text.contains("target_model"));
}
