//! Best-effort secret redaction for telemetry payloads.
//!
//! Unlike the AI-side secret redaction in `app/src/ai/blocklist/block/secret_redaction.rs`,
//! which is gated on the user's secret-redaction (a.k.a. "safe mode") setting and is used
//! for visual obfuscation in the terminal, the redaction in this module is unconditional:
//! we always do a redaction pass on telemetry payloads that may contain user-generated
//! content, regardless of the user's safe-mode setting. The two settings are deliberately
//! decoupled — visual obfuscation is a UX preference, while telemetry-side redaction is a
//! defence-in-depth measure for data leaving the device.
//!
//! The regex used for redaction always includes the default patterns defined in
//! `crate::terminal::model::secrets::regexes::DEFAULT_REGEXES_WITH_NAMES`. Any custom
//! patterns the user has configured (or that their organization has configured via
//! enterprise secret redaction) are layered on top of those defaults.
//!
//! This module is intentionally lightweight: it does byte-range matching only and does
//! not track `SecretLevel`s or character ranges, since the telemetry path doesn't need
//! either.
use std::collections::HashSet;
use std::ops::Range;

use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex_automata::meta::Regex;
use serde_json::Value;

use crate::terminal::model::secrets::regexes::DEFAULT_REGEXES_WITH_NAMES;
const REDACTION_REPLACEMENT_CHARACTER: &str = "*";
lazy_static! {
    /// Regex used to redact secrets from telemetry payloads. Initialized with the
    /// default patterns so that redaction works even before the user's privacy
    /// settings are loaded (and even for users who have never configured any
    /// custom patterns).
    static ref TELEMETRY_SECRETS_REGEX: RwLock<Regex> = RwLock::new(build_default_regex());
}
