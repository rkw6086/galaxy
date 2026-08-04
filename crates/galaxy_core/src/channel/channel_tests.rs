use super::Channel;

#[test]
fn local_control_channel_names_do_not_expose_legacy_branding() {
    assert_eq!(Channel::Stable.local_control_channel_name(), "stable");
    assert_eq!(Channel::Preview.local_control_channel_name(), "preview");
    assert_eq!(Channel::Dev.local_control_channel_name(), "dev");
    assert_eq!(Channel::Local.local_control_channel_name(), "local");
    assert_eq!(
        Channel::Integration.local_control_channel_name(),
        "integration"
    );
    assert_eq!(Channel::Oss.local_control_channel_name(), "oss");
}

#[test]
fn only_oss_disables_warp_service_egress() {
    assert!(Channel::Stable.allows_warp_service_egress());
    assert!(Channel::Preview.allows_warp_service_egress());
    assert!(Channel::Dev.allows_warp_service_egress());
    assert!(Channel::Local.allows_warp_service_egress());
    assert!(Channel::Integration.allows_warp_service_egress());
    assert!(!Channel::Oss.allows_warp_service_egress());
}
