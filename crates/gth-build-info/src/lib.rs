//! The harness build, recorded in every dataset's metadata (`meta.toml`'s `[harness]`, a
//! shard's `harness`): `_native.build_info()` in Python and the same map in `gth-play`, from
//! this one function (docs/native.md, "Build info"). `build.rs` captures the values.

use std::collections::BTreeMap;

/// Package version, git commit and whether the Rust sources differed from it, target triple,
/// cargo profile and rustc version. Every `gth-*` crate shares the workspace version. Sorted
/// by key, so serialized metadata is stable.
pub fn build_info() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("version", env!("CARGO_PKG_VERSION")),
        ("git_sha", env!("GTH_GIT_SHA")),
        ("git_dirty", env!("GTH_GIT_DIRTY")),
        ("target", env!("GTH_TARGET")),
        ("profile", env!("GTH_PROFILE")),
        ("rustc", env!("GTH_RUSTC")),
    ])
}

/// [`build_info`] with owned strings, as `gth_record::RecordingInfo::harness` takes it.
pub fn build_info_owned() -> BTreeMap<String, String> {
    build_info()
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_the_recorded_keys() {
        let info = build_info();
        let keys: Vec<_> = info.keys().copied().collect();
        assert_eq!(
            keys,
            [
                "git_dirty",
                "git_sha",
                "profile",
                "rustc",
                "target",
                "version"
            ]
        );
        assert_eq!(build_info_owned().len(), keys.len());
        assert!(info["target"].split('-').count() >= 3, "a target triple");
    }
}
