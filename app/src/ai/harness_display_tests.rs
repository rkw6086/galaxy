use galaxy_cli::agent::Harness;

use super::display_name;

#[test]
fn oz_harness_uses_galaxy_display_name() {
    assert_eq!(display_name(Harness::Oz), "Galaxy");
}
