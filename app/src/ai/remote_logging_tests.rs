use serde_json::json;

use super::{normalize_endpoint_url, sanitize_error, tail_limited_payload_context};

#[test]
fn endpoint_accepts_base_or_logs_path() {
    assert_eq!(
        normalize_endpoint_url("https://logging.ryserve.net").as_deref(),
        Some("https://logging.ryserve.net/api/logs")
    );
    assert_eq!(
        normalize_endpoint_url("https://logging.ryserve.net/api/logs/").as_deref(),
        Some("https://logging.ryserve.net/api/logs")
    );
}

#[test]
fn errors_are_compacted_truncated_and_lightly_redacted() {
    let error = format!("failed\nAuthorization: Bearer sk-test {}", "x".repeat(700));
    let sanitized = sanitize_error(error);

    assert!(!sanitized.contains("sk-test"));
    assert!(!sanitized.contains('\n'));
    assert!(sanitized.chars().count() <= 501);
}

#[test]
fn raw_payload_cap_keeps_tail() {
    assert_eq!(
        tail_limited_payload_context("0123456789", 4),
        json!({
            "payload": "6789",
            "payload_total_chars": 10,
            "payload_included_chars": 4,
            "payload_max_chars": 4,
            "truncated": true,
            "truncation_strategy": "tail",
        })
    );
}

#[test]
fn streamed_payload_matches_existing_tail_for_unicode_and_split_writes() {
    let payload = "start \"escaped\" é日本🙂 ending".repeat(100);
    for limit in [0, 1, 3, 17, payload.chars().count(), payload.len()] {
        for chunk_size in [1, 2, 7, payload.len()] {
            let actual = super::bounded_payload_context(limit, |writer| {
                for chunk in payload.as_bytes().chunks(chunk_size) {
                    writer.write_all(chunk).unwrap();
                }
                Ok(())
            });
            assert_eq!(actual, tail_limited_payload_context(&payload, limit));
        }
    }
}

#[test]
fn streamed_json_matches_serialized_payload_without_materializing_the_archive() {
    let payload = json!({ "archive": ["large tool output".repeat(1000)], "input": "steer 🙂" });
    let expected = serde_json::to_string_pretty(&payload).unwrap();
    let actual = super::bounded_payload_context(100, |writer| {
        serde_json::to_writer_pretty(writer, &payload)
    });
    assert_eq!(actual, tail_limited_payload_context(&expected, 100));
}

#[test]
fn prefix_payload_preserves_utf8_and_reports_unknown_truncated_size() {
    let payload = "é日本🙂 ending";
    for limit in [0, 1, 3, 4, payload.chars().count(), payload.len()] {
        for chunk_size in [1, 2, 7, payload.len()] {
            let actual = super::prefix_bounded_payload_context(limit, |writer| {
                for chunk in payload.as_bytes().chunks(chunk_size) {
                    writer.write_all(chunk).map_err(serde_json::Error::io)?;
                }
                Ok(())
            });
            let expected = payload.chars().take(limit).collect::<String>();
            let truncated = limit < payload.chars().count();
            assert_eq!(actual["payload"], expected);
            assert_eq!(actual["payload_included_chars"], expected.chars().count());
            assert_eq!(actual["truncated"], truncated);
            assert_eq!(actual["serialization_stopped_early"], truncated);
            assert_eq!(
                actual["payload_total_chars"],
                if truncated {
                    serde_json::Value::Null
                } else {
                    json!(payload.chars().count())
                }
            );
        }
    }
}

#[test]
fn prefix_payload_stops_pretty_debug_serialization_at_the_budget() {
    use std::cell::Cell;

    struct LargeDebug<'a>(&'a Cell<usize>);
    impl std::fmt::Debug for LargeDebug<'_> {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for _ in 0..1_000_000 {
                self.0.set(self.0.get() + 1);
                formatter.write_str("large archive entry ")?;
            }
            Ok(())
        }
    }
    struct DebugPayload<'a>(LargeDebug<'a>);
    impl serde::Serialize for DebugPayload<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_str(&format_args!("{:#?}", self.0))
        }
    }

    let visited = Cell::new(0);
    let actual = super::prefix_bounded_payload_context(64, |writer| {
        serde_json::to_writer(writer, &DebugPayload(LargeDebug(&visited)))
    });

    assert!(visited.get() < 10);
    assert_eq!(actual["payload_included_chars"], 64);
    assert_eq!(actual["truncated"], true);
    assert_eq!(actual["payload_total_chars"], serde_json::Value::Null);
    assert_eq!(actual["truncation_strategy"], "prefix");
}

#[test]
fn prefix_payload_keeps_real_serialization_errors_visible() {
    let actual = super::prefix_bounded_payload_context(100, |_writer| {
        Err(serde_json::Error::io(std::io::Error::other(
            "test serializer failed",
        )))
    });
    assert!(actual["serialization_error"]
        .as_str()
        .unwrap()
        .contains("test serializer failed"));
    assert!(actual.get("payload").is_none());
}
