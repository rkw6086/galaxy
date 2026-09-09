use std::ffi::OsStr;

use super::env_value_is_enabled;

#[test]
fn diagnostic_env_accepts_only_explicit_true_values() {
    assert!(env_value_is_enabled(Some(OsStr::new("1"))));
    assert!(env_value_is_enabled(Some(OsStr::new("TRUE"))));
    assert!(!env_value_is_enabled(Some(OsStr::new("0"))));
    assert!(!env_value_is_enabled(Some(OsStr::new("yes"))));
    assert!(!env_value_is_enabled(None));
}
