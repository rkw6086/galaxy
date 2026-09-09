use super::*;

#[test]
fn test_is_approved_exact() {
    assert!(is_approved("LGTM!"));
}

#[test]
fn test_is_approved_with_whitespace() {
    assert!(is_approved("  LGTM!  "));
    assert!(is_approved("\nLGTM!\n"));
}

#[test]
fn test_is_approved_case_insensitive() {
    assert!(is_approved("lgtm!"));
    assert!(is_approved("Lgtm!"));
    assert!(is_approved("lgtm"));
}

#[test]
fn test_is_not_approved_with_feedback() {
    assert!(!is_approved("LGTM! But also fix the typo."));
    assert!(!is_approved("1. Fix the loop\n2. Rename variable"));
    assert!(!is_approved(""));
}
