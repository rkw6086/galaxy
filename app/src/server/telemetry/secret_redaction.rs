use regex::Regex;

pub fn update_telemetry_secrets_regex<'a>(
    _user_secrets: impl Iterator<Item = &'a Regex>,
    _enterprise_secrets: impl Iterator<Item = &'a Regex>,
) {
}
