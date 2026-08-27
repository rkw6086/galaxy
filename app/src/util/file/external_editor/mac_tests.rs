use super::is_galaxy_bundle;

#[test]
fn is_galaxy_bundle_recognises_oss() {
    assert!(is_galaxy_bundle("com.galaxy.GalaxyOss"));
}

#[test]
fn is_galaxy_bundle_rejects_other_apps() {
    assert!(!is_galaxy_bundle("com.microsoft.VSCode"));
    assert!(!is_galaxy_bundle("com.apple.TextEdit"));
    assert!(!is_galaxy_bundle("dev.warp.WarpOss"));
    assert!(!is_galaxy_bundle("invalid"));
    assert!(!is_galaxy_bundle(""));
}
