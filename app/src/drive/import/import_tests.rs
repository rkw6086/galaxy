use std::env::current_dir;

use galaxyui::App;

use super::expand_dirs;

#[test]
fn test_expand_directories() {
    App::test((), |_| async move {
        let directory = current_dir()
            .expect("current directory should exist")
            .parent()
            .expect("parent directory should exist")
            .to_path_buf()
            .join("crates")
            .join("integration");

        // Open a folder and verify we could expand it into the correct folder tree structure.
        assert_eq!(galaxyui::r#async::block_on(expand_dirs([directory].into_iter().collect())).debug_print(), "(integration(tests(INTEGRATION_TESTING, data(test, test_launch_config, test_theme, test_theme_with_name, test_workflow))))");
    });
}
