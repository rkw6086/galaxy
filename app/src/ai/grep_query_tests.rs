use super::prepare_grep_queries;

#[test]
fn literal_queries_match_source_punctuation_backslashes_and_unicode_exactly() {
    let cases = [
        ("variantResolutions: []", r"variantResolutions: \[\]"),
        ("indexAsset(context", r"indexAsset\(context"),
        (
            r#"describe("duplicate"|managed output|duplicateContentAsset"#,
            r#"describe\("duplicate"\|managed output\|duplicateContentAsset"#,
        ),
        (r"C:\repo\file.rs", r"C:\\repo\\file\.rs"),
        ("🚀 café [数据] (名)", r"🚀 café \[数据\] \(名\)"),
        (
            r#"owner's $HOME `id` #&-~ name=foo: bar"#,
            r#"owner's \$HOME `id` #&-~ name=foo: bar"#,
        ),
        (
            "^foo.*(bar|baz)?[0-9]+{2}$",
            r"\^foo\.\*\(bar\|baz\)\?\[0-9\]\+\{2\}\$",
        ),
    ];
    for (literal, escaped) in cases {
        let queries = prepare_grep_queries(vec![literal.to_string()], true);
        assert_eq!(queries, vec![escaped], "literal: {literal}");
        let regex = regex::Regex::new(&queries[0]).unwrap();
        assert_eq!(regex.find(literal).unwrap().as_str(), literal);
    }
}

#[test]
fn regex_queries_stay_byte_for_byte_unchanged_including_backend_specific_syntax() {
    let queries = vec![
        r"^(foo|bar)\s+[0-9]+$".to_string(),
        r"(?<=prefix)lookbehind".to_string(),
        "[invalid".to_string(),
    ];
    assert_eq!(prepare_grep_queries(queries.clone(), false), queries);
}
