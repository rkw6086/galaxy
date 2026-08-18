use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::sync::OnceLock;

const ENV_VAR: &str = "GALAXY_TOOL_DIAGNOSTICS";

fn env_value_is_enabled(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn is_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| env_value_is_enabled(std::env::var_os(ENV_VAR).as_deref()))
}

pub(crate) fn capture_backtrace() -> Option<Backtrace> {
    is_enabled().then(Backtrace::force_capture)
}

macro_rules! tool_debug {
    ($($arg:tt)*) => {
        if $crate::ai::tool_diagnostics::is_enabled() {
            log::debug!("[tool-debug] {}", format_args!($($arg)*));
        }
    };
}

pub(crate) use tool_debug;

#[cfg(test)]
mod tests {
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
}
