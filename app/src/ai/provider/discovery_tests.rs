use super::model_availability_is_usable;

#[test]
fn requires_every_availability_status() {
    assert!(model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        None,
        "AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "NOT_AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "NOT_AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "AVAILABLE",
        "NOT_AVAILABLE",
    ));
}
