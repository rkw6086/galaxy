pub(crate) const SOURCE_COMMIT: &str = env!("GALAXY_BUILD_GIT_COMMIT");

pub(crate) fn source_is_dirty() -> bool {
    env!("GALAXY_BUILD_GIT_DIRTY") == "true"
}

pub(crate) fn source_revision_label() -> String {
    if source_is_dirty() {
        format!("{SOURCE_COMMIT} (modified)")
    } else {
        SOURCE_COMMIT.to_owned()
    }
}

pub(crate) const fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
