//! Path arithmetic the harness does textually.

use std::path::{Component, Path, PathBuf};

/// Resolve `path` against `base`, folding away `.` and `..` textually.
///
/// Not a canonicalization: no symlink is followed, because `CARGO_MANIFEST_DIR`
/// is not canonical either and the two must stay comparable for normalization to
/// match.
///
/// The folding is load-bearing rather than cosmetic. Cargo normalizes the
/// `manifest_path` it reports, so a `..` left in the path the harness hands it
/// comes back spelled differently -- and every message is attributed by
/// comparing those two paths.
pub(crate) fn lexical_join(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };

    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                // A real directory name to cancel.
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // There is nothing above the root: `/..` is `/`, and cargo
                // resolves it that way too. Matching on `RootDir` alone is
                // enough for a Windows rooted path, which is a `Prefix`
                // followed by a `RootDir` -- while a drive-relative `C:..`,
                // which has the prefix and no root, correctly keeps its `..`.
                Some(Component::RootDir) => {}
                // Nothing to cancel. Dropping it would name a different
                // directory, so it stays.
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_parent_components() {
        assert_eq!(
            lexical_join(Path::new("/w/crate"), Path::new("..")),
            Path::new("/w")
        );
        assert_eq!(
            lexical_join(Path::new("/w/crate"), Path::new("../other")),
            Path::new("/w/other")
        );
        assert_eq!(
            lexical_join(Path::new("/w"), Path::new("./tests/ui")),
            Path::new("/w/tests/ui")
        );
    }

    #[test]
    fn keeps_absolute_paths() {
        assert_eq!(
            lexical_join(Path::new("/w"), Path::new("/elsewhere")),
            Path::new("/elsewhere")
        );
    }

    /// An absolute path is folded too. Cargo reports `/a/b/../c` as `/a/c`, so
    /// leaving the `..` in would be leaving the mismatch in.
    #[test]
    fn folds_an_absolute_path_it_was_handed() {
        assert_eq!(
            lexical_join(Path::new("/w"), Path::new("/a/b/../c")),
            Path::new("/a/c")
        );
    }

    /// A `..` with nothing to cancel is kept rather than dropped: dropping it
    /// would silently point at a different directory.
    #[test]
    fn keeps_a_parent_component_it_cannot_cancel() {
        assert_eq!(
            lexical_join(Path::new(""), Path::new("../a")),
            Path::new("../a")
        );
    }

    /// A `..` that reaches the root is absorbed by it. Leaving `/..` in would
    /// leave exactly the kind of unfolded parent this exists to remove: cargo
    /// resolves `/..` to `/` and would answer with a path that no longer
    /// compares equal.
    #[test]
    fn a_parent_component_stops_at_the_root() {
        assert_eq!(
            lexical_join(Path::new("/"), Path::new("..")),
            Path::new("/")
        );
        assert_eq!(
            lexical_join(Path::new("/a"), Path::new("../..")),
            Path::new("/")
        );
        assert_eq!(
            lexical_join(Path::new("/a"), Path::new("../../b")),
            Path::new("/b")
        );
    }
}
