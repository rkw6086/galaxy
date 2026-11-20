pub(crate) const SOURCE_COMMIT: &str = env!("GALAXY_BUILD_GIT_COMMIT");

pub(crate) fn build_channel() -> &'static str {
    env!("GALAXY_BUILD_CHANNEL")
}

pub(crate) fn nightly_id() -> Option<&'static str> {
    option_env!("GALAXY_BUILD_NIGHTLY_ID").filter(|value| !value.is_empty())
}

pub(crate) fn artifact_identity() -> Option<&'static str> {
    option_env!("GALAXY_BUILD_ARTIFACT_IDENTITY").filter(|value| !value.is_empty())
}

pub(crate) fn official_github_build() -> bool {
    option_env!("GALAXY_BUILD_OFFICIAL_GITHUB_CI") == Some("1")
        && !cfg!(debug_assertions)
        && !source_is_dirty()
}

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

pub(crate) fn about_version_label() -> String {
    match build_channel() {
        "nightly" => format!("Nightly · {}", source_revision_label()),
        "beta" => format!("Beta · {}", source_revision_label()),
        _ => format!("Version {}", env!("CARGO_PKG_VERSION")),
    }
}

pub(crate) const fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
