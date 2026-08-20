use dirs::home_dir;

use super::*;

#[test]
fn test_data_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(data_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(data_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(windows)] {
            assert_eq!(data_dir(), home_dir.join("AppData\\Roaming\\galaxy\\Galaxy\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_config_local_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(config_local_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(config_local_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(windows)] {
            assert_eq!(config_local_dir(), home_dir.join("AppData\\Local\\galaxy\\Galaxy\\config"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_galaxy_home_config_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    let expected_dir_name = match ChannelState::data_profile() {
        Some(data_profile) => format!(".galaxy-{data_profile}"),
        None => ".galaxy".to_string(),
    };

    assert_eq!(
        galaxy_home_config_dir(),
        Some(home_dir.join(expected_dir_name))
    );
}

#[test]
fn test_warp_home_skills_and_mcp_paths() {
    let Some(config_dir) = galaxy_home_config_dir() else {
        panic!("Should be able to compute Warp home config directory");
    };

    assert_eq!(galaxy_home_skills_dir(), Some(config_dir.join("skills")));
    assert_eq!(
        galaxy_home_mcp_config_file_path(),
        Some(config_dir.join(".mcp.json"))
    );
}
#[test]
fn test_cache_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(cache_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(cache_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(windows)] {
            assert_eq!(cache_dir(), home_dir.join("AppData\\Local\\galaxy\\Galaxy\\cache"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_state_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    cfg_if::cfg_if! {
        // ChannelState, by default, is configured for Channel::Oss.
        if #[cfg(target_os = "macos")] {
            assert_eq!(state_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(state_dir(), home_dir.join(".galaxy"));
        } else if #[cfg(windows)] {
            assert_eq!(state_dir(), home_dir.join("AppData\\Local\\galaxy\\Galaxy\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_migrate_legacy_macos_data_dir_merges_without_overwriting() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let source_dir = tempdir.path().join("legacy");
    let target_dir = tempdir.path().join(".galaxy");
    std::fs::create_dir_all(source_dir.join("nested")).expect("source should be created");
    std::fs::write(source_dir.join("galaxy.sqlite"), b"legacy database")
        .expect("source file should be created");
    std::fs::write(source_dir.join("nested/legacy.txt"), b"legacy content")
        .expect("nested source file should be created");
    std::fs::create_dir_all(target_dir.join("nested")).expect("target should be created");
    std::fs::write(target_dir.join("galaxy.sqlite"), b"current database")
        .expect("target file should be created");
    std::fs::write(target_dir.join("nested/current.txt"), b"current content")
        .expect("nested target file should be created");

    migrate_directory_contents(&source_dir, &target_dir);

    assert_eq!(
        std::fs::read(target_dir.join("galaxy.sqlite")).unwrap(),
        b"current database"
    );
    assert_eq!(
        std::fs::read(target_dir.join("nested/legacy.txt")).unwrap(),
        b"legacy content"
    );
    assert_eq!(
        std::fs::read(target_dir.join("nested/current.txt")).unwrap(),
        b"current content"
    );
    // The source dir should still exist because "galaxy.sqlite" conflicted
    // and was left in place.
    assert!(source_dir.join("galaxy.sqlite").exists());
    // The nested directory was fully merged (legacy.txt moved) and removed.
    assert!(!source_dir.join("nested").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn test_migrate_legacy_macos_data_dir_keeps_conflicting_legacy_data() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let source_dir = tempdir.path().join("legacy");
    let target_dir = tempdir.path().join(".galaxy");
    std::fs::create_dir_all(&source_dir).expect("source should be created");
    std::fs::create_dir_all(&target_dir).expect("target should be created");
    std::fs::write(source_dir.join("settings.json"), b"legacy")
        .expect("source file should be created");
    std::fs::write(target_dir.join("settings.json"), b"current")
        .expect("target file should be created");

    migrate_directory_contents(&source_dir, &target_dir);

    assert_eq!(
        std::fs::read(target_dir.join("settings.json")).unwrap(),
        b"current"
    );
    assert_eq!(
        std::fs::read(source_dir.join("settings.json")).unwrap(),
        b"legacy"
    );
}

#[test]
fn test_project_path_for_warp_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "warp", "Warp"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "com.galaxy.Galaxy");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "galaxy");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "galaxy\\Galaxy");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_warp_dev_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "warp", "WarpDev"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "com.galaxy.Galaxy");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "galaxy");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "galaxy\\Galaxy");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_oss_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "warp", "WarpOss"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "com.galaxy.Galaxy");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "galaxy");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "galaxy\\Galaxy");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}
