//! Making diagnostics machine-independent.
//!
//! Every substitution here is a thing a golden can no longer distinguish, so the
//! list is deliberately short and deliberately closed. Five substitutions is a
//! design; fifteen is a symptom that the goldens are recording things they
//! should not.

use std::env;
use std::path::Path;

/// Placeholder for the host crate's manifest directory.
pub(crate) const DIR: &str = "$DIR";
/// Placeholder for the harness's own scratch project.
pub(crate) const SCRATCH: &str = "$SCRATCH";
/// Placeholder for an unpacked registry source directory.
pub(crate) const CARGO_REGISTRY: &str = "$CARGO_REGISTRY";
/// Placeholder for `CARGO_HOME` itself.
pub(crate) const CARGO_HOME: &str = "$CARGO_HOME";

/// The path rewrites for one run. Built once and reused for every fixture; only
/// [`Normalizer::normalize`]'s `fixture` argument changes between them.
pub(crate) struct Normalizer {
    /// Absolute path of the scratch project's `src/main.rs`.
    scratch_main: String,
    /// Absolute path of the scratch root, covering both the generated project
    /// and its private target directory.
    scratch_root: String,
    /// Absolute path of the host crate's manifest directory.
    manifest_dir: String,
    /// `CARGO_HOME`, if it can be determined.
    cargo_home: Option<String>,
}

impl Normalizer {
    pub(crate) fn new(scratch_root: &Path, scratch_main: &Path, manifest_dir: &Path) -> Self {
        Self {
            scratch_main: scratch_main.display().to_string(),
            scratch_root: scratch_root.display().to_string(),
            manifest_dir: manifest_dir.display().to_string(),
            cargo_home: cargo_home().map(|home| home.display().to_string()),
        }
    }

    /// Rewrite `text` so it says the same thing on any machine.
    ///
    /// `fixture` is the fixture's path relative to the host manifest directory:
    /// the scratch project's `src/main.rs` is rewritten to it, which is what
    /// makes a golden readable and what lets `trybuild` goldens migrate.
    pub(crate) fn normalize(&self, text: &str, fixture: &str) -> String {
        let mut lines: Vec<String> = Vec::new();

        for line in text.lines() {
            // `str::lines` splits on `\n` and drops a trailing `\r`, so CRLF is
            // handled here. rustc emits trailing spaces on some continuation
            // lines, and they are invisible in a diff.
            let line = line.trim_end();

            // Absolute scratch paths first: the scratch main path *ends* in
            // `src/main.rs`, so rewriting the relative form first would corrupt
            // it, and the scratch root is a prefix of it.
            let line = line.replace(&self.scratch_main, fixture);
            let line = line.replace(&self.scratch_root, SCRATCH);
            // Cargo reports paths in the package under compilation relative to
            // that package's root, so the scratch main appears bare as
            // `src/main.rs`. Anchored to a span header rather than replaced
            // globally: a fixture is free to contain the literal text
            // `src/main.rs`, and a path dependency is free to have a file of
            // that name, and rewriting either to the fixture path would put a
            // lie in the golden.
            let line = rewrite_span_target(&line, "src/main.rs", fixture);

            let line = match &self.cargo_home {
                Some(home) => {
                    let line = replace_registry_src(&line, home);
                    line.replace(home.as_str(), CARGO_HOME)
                }
                None => line,
            };

            // Last, because the scratch root usually lives *inside* the host
            // crate's target directory; rewriting the manifest dir first would
            // stop the scratch substitutions from ever matching.
            let line = line.replace(&self.manifest_dir, DIR);

            lines.push(line);
        }

        let mut out = lines.join("\n");
        while out.ends_with('\n') {
            out.pop();
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}

/// Rewrite the target of a `--> ` span header, and only there.
///
/// The target runs to the `:line:col` suffix, so a match must be followed by a
/// colon or end the line -- otherwise `src/main.rs` would also match a longer
/// path that merely starts with it.
fn rewrite_span_target(line: &str, from: &str, to: &str) -> String {
    let Some(arrow) = line.find("--> ") else {
        return line.to_string();
    };
    let (head, target) = line.split_at(arrow + "--> ".len());
    match target.strip_prefix(from) {
        Some(rest) if rest.is_empty() || rest.starts_with(':') => format!("{head}{to}{rest}"),
        _ => line.to_string(),
    }
}

/// Rewrite `<cargo_home>/registry/src/<index>/` to `$CARGO_REGISTRY/`.
///
/// The index component carries a hash that varies by machine and by cargo
/// version, so it is consumed along with the prefix.
fn replace_registry_src(line: &str, cargo_home: &str) -> String {
    let prefix = format!("{}/registry/src/", cargo_home.trim_end_matches('/'));
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(at) = rest.find(&prefix) {
        out.push_str(&rest[..at]);
        out.push_str(CARGO_REGISTRY);
        out.push('/');
        let after = &rest[at + prefix.len()..];
        // Consume the index directory component. With no separator after it
        // there is no component to consume, and dropping the remainder would
        // silently truncate the line.
        rest = match after.find('/') {
            Some(slash) => &after[slash + 1..],
            None => after,
        };
    }
    out.push_str(rest);
    out
}

/// `CARGO_HOME` if set, else the conventional `~/.cargo`.
fn cargo_home() -> Option<std::path::PathBuf> {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Some(home.into());
    }
    env::var_os("HOME").map(|home| Path::new(&home).join(".cargo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn normalizer() -> Normalizer {
        Normalizer {
            scratch_main: "/w/target/nobuild/host/project/src/main.rs".into(),
            scratch_root: "/w/target/nobuild/host".into(),
            manifest_dir: "/w".into(),
            cargo_home: Some("/home/u/.cargo".into()),
        }
    }

    #[test]
    fn rewrites_the_scratch_main_to_the_fixture() {
        let out = normalizer().normalize(" --> src/main.rs:4:9\n", "tests/ui/a.rs");
        assert_eq!(out, " --> tests/ui/a.rs:4:9\n");
    }

    #[test]
    fn rewrites_the_absolute_scratch_main_to_the_fixture() {
        let out = normalizer().normalize(
            "note: at /w/target/nobuild/host/project/src/main.rs:1:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, "note: at tests/ui/a.rs:1:1\n");
    }

    #[test]
    fn rewrites_the_scratch_root_before_the_manifest_dir() {
        let out = normalizer().normalize("note: /w/target/nobuild/host/target/debug\n", "f.rs");
        assert_eq!(out, "note: $SCRATCH/target/debug\n");
    }

    #[test]
    fn rewrites_the_host_manifest_dir() {
        let out = normalizer().normalize("note: /w/src/lib.rs:9:1\n", "f.rs");
        assert_eq!(out, "note: $DIR/src/lib.rs:9:1\n");
    }

    #[test]
    fn rewrites_registry_paths_without_their_index_hash() {
        let out = normalizer().normalize(
            "note: /home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dep-1.0/src/x.rs:3:1\n",
            "f.rs",
        );
        assert_eq!(out, "note: $CARGO_REGISTRY/dep-1.0/src/x.rs:3:1\n");
    }

    #[test]
    fn rewrites_other_cargo_home_paths() {
        let out = normalizer().normalize("note: /home/u/.cargo/git/checkouts/x\n", "f.rs");
        assert_eq!(out, "note: $CARGO_HOME/git/checkouts/x\n");
    }

    #[test]
    fn leaves_the_fixtures_own_source_text_alone() {
        // The snippet quotes the fixture. Rewriting inside it would misquote the
        // code under test and misalign the carets beneath.
        let out = normalizer().normalize("2 |     let _x = \"src/main.rs\";\n", "tests/ui/a.rs");
        assert_eq!(out, "2 |     let _x = \"src/main.rs\";\n");
    }

    #[test]
    fn leaves_a_dependencys_own_main_alone() {
        let out = normalizer().normalize(" --> /w/dep/src/main.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/dep/src/main.rs:1:1\n");
    }

    #[test]
    fn registry_paths_without_a_trailing_component_keep_their_text() {
        let out = normalizer().normalize("note: /home/u/.cargo/registry/src/index-abc\n", "f.rs");
        assert_eq!(out, "note: $CARGO_REGISTRY/index-abc\n");
    }

    #[test]
    fn strips_trailing_whitespace_and_carriage_returns() {
        let out = normalizer().normalize("error: x   \r\n  |   \r\n", "f.rs");
        assert_eq!(out, "error: x\n  |\n");
    }

    #[test]
    fn ends_with_exactly_one_newline() {
        assert_eq!(
            normalizer().normalize("error: x\n\n\n", "f.rs"),
            "error: x\n"
        );
        assert_eq!(normalizer().normalize("error: x", "f.rs"), "error: x\n");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(normalizer().normalize("", "f.rs"), "");
    }

    #[test]
    fn new_reads_the_paths_it_is_given() {
        let n = Normalizer::new(
            &PathBuf::from("/s"),
            &PathBuf::from("/s/project/src/main.rs"),
            &PathBuf::from("/h"),
        );
        assert_eq!(n.scratch_root, "/s");
        assert_eq!(n.scratch_main, "/s/project/src/main.rs");
        assert_eq!(n.manifest_dir, "/h");
    }
}
