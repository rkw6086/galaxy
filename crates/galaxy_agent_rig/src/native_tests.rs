use super::native_descriptor;

#[test]
fn native_descriptors_are_provider_specific() {
    let descriptor = native_descriptor("anthropic", "claude-sonnet");

    assert_eq!(descriptor.id, "rig-anthropic:claude-sonnet");
    assert_eq!(descriptor.display_name, "Rig / anthropic / claude-sonnet");
}
