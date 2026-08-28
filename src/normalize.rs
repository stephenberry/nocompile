//! Making diagnostics machine-independent.
//!
//! Every substitution here is a thing a golden can no longer distinguish, so the
//! fixed list is deliberately short and deliberately closed. Five placeholders
//! is a design; fifteen is a symptom that the goldens are recording things they
//! should not.
//!
//! The one open-ended part is intentional: a declared path dependency can get a
//! placeholder of its own, because a diagnostic is free to point into a
//! dependency's source and that path is absolute and machine-specific. Those
//! placeholders are not a growing list of special cases -- they are one rule
//! applied to whatever the caller declared. The rule runs last and so only
//! claims what the fixed placeholders left alone: a dependency inside the host
//! crate stays under `$DIR`, and a vendored one stays under `$CARGO_HOME`, both
//! of which are already portable.
//!
//! # Line numbers
//!
//! Only the fixture's own spans keep their `:line:col`. A span pointing into any
//! other file loses them, along with the line numbers in the snippet beneath it.
//! Those numbers record where a *dependency* happens to put its code today:
//! adding a doc comment near the top of a dependency file would otherwise
//! re-bless every golden whose diagnostic reaches into it, for a reason that has
//! nothing to do with the invariant under test.

use crate::scratch::Dependency;
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
/// Placeholder for the toolchain's own source, wherever it is unpacked.
pub(crate) const RUST: &str = "$RUST";

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
    /// One `(absolute path, placeholder)` per declared path dependency, longest
    /// path first.
    dependencies: Vec<(String, String)>,
}

impl Normalizer {
    pub(crate) fn new(
        scratch_root: &Path,
        scratch_main: &Path,
        manifest_dir: &Path,
        dependencies: &[Dependency],
    ) -> Self {
        let mut dependencies: Vec<(String, String)> = dependencies
            .iter()
            .map(|dep| (dep.path.display().to_string(), placeholder(&dep.name)))
            .collect();
        // Longest path first, so a dependency nested inside another claims its
        // own paths rather than having the outer one rewrite the prefix and
        // leave a half-substituted line behind.
        dependencies.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));

        Self {
            scratch_main: scratch_main.display().to_string(),
            scratch_root: scratch_root.display().to_string(),
            manifest_dir: manifest_dir.display().to_string(),
            cargo_home: cargo_home().map(|home| home.display().to_string()),
            dependencies,
        }
    }

    /// Rewrite `text` so it says the same thing on any machine.
    ///
    /// `fixture` is the fixture's path relative to the host manifest directory:
    /// the scratch project's `src/main.rs` is rewritten to it, which is what
    /// makes a golden readable and what lets `trybuild` goldens migrate.
    pub(crate) fn normalize(&self, text: &str, fixture: &str) -> String {
        let mut lines: Vec<String> = Vec::new();
        // Whether the rows still arriving belong to the snippet under a span
        // that pointed outside the fixture.
        let mut in_foreign_snippet = false;

        for line in text.lines() {
            // `str::lines` splits on `\n` and drops a trailing `\r`, so CRLF is
            // handled here. rustc emits trailing spaces on some continuation
            // lines, and they are invisible in a diff.
            let mut line = line.trim_end().to_string();

            if in_foreign_snippet {
                if is_snippet_row(&line) {
                    blank_leading_number(&mut line);
                } else {
                    in_foreign_snippet = false;
                }
            }

            line = self.rewrite_paths(&line, fixture);

            // Done after rewriting, so the comparison is against the fixture's
            // normalized path rather than the scratch project's.
            if let Some(target) = span_target(&line) {
                in_foreign_snippet = !points_into(target, fixture);
                if in_foreign_snippet {
                    hide_trailing_numbers(&mut line);
                }
            }

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

    /// Replace every machine-specific path in one line with its placeholder.
    fn rewrite_paths(&self, line: &str, fixture: &str) -> String {
        // Absolute scratch paths first: the scratch main path *ends* in
        // `src/main.rs`, so rewriting the relative form first would corrupt
        // it, and the scratch root is a prefix of it.
        let line = replace_dir(line, &self.scratch_main, fixture);
        let line = replace_dir(&line, &self.scratch_root, SCRATCH);
        // Cargo reports paths in the package under compilation relative to
        // that package's root, so the scratch main appears bare as
        // `src/main.rs`. Anchored to a span header rather than replaced
        // globally: a fixture is free to contain the literal text
        // `src/main.rs`, and a path dependency is free to have a file of
        // that name, and rewriting either to the fixture path would put a
        // lie in the golden.
        let line = rewrite_span_target(&line, "src/main.rs", fixture);

        // Before `CARGO_HOME`, which is a sibling of the toolchain directory
        // rather than a parent of it, so the two never compete -- but the
        // sysroot is the more specific rule and reads better first.
        let line = replace_sysroot(&line);

        let line = match &self.cargo_home {
            Some(home) => {
                let line = replace_registry_src(&line, home);
                replace_dir(&line, home, CARGO_HOME)
            }
            None => line,
        };

        // The manifest dir comes late, because the scratch root usually lives
        // *inside* the host crate's target directory; rewriting the manifest dir
        // first would stop the scratch substitutions from ever matching.
        let mut line = replace_dir(&line, &self.manifest_dir, DIR);

        // Dependencies come last, and so only ever claim a path the earlier
        // rules left alone. A dependency inside the host crate is already
        // `$DIR/...` by now and stays that way; the placeholders exist for the
        // ones that sit outside it, where nothing else covers the path. A
        // sibling checkout is the common case and the one that makes a golden
        // unshareable.
        for (path, name) in &self.dependencies {
            line = replace_dir(&line, path, name);
        }
        line
    }
}

/// Replace the directory `from` with `to`, but only where `from` is a whole path
/// prefix rather than the start of a longer name.
///
/// A plain `str::replace` rewrites `/w/crate` inside `/w/crate-helper`, turning
/// a sibling checkout into `$DIR-helper`: a path that is neither the real one
/// nor a portable one. The sibling checkout is exactly the case these
/// placeholders exist for, so the prefix has to be anchored.
fn replace_dir(line: &str, from: &str, to: &str) -> String {
    // `rest.find("")` returns `Some(0)` without consuming anything, so an empty
    // `from` would spin forever rather than merely doing nothing.
    if from.is_empty() {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut consumed = 0;
    while let Some(at) = rest.find(from) {
        let after = &rest[at + from.len()..];
        let anchored = ends_component(after.chars().next())
            && starts_component(line[..consumed + at].chars().next_back());

        out.push_str(&rest[..at]);
        out.push_str(if anchored { to } else { from });
        consumed += at + from.len();
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether the character *before* a match lets it start a path.
///
/// Nothing at all (the line begins with the path) counts, as does any character
/// that cannot be part of one. A path character means the match landed inside a
/// longer path -- `/w` inside `/opt/w/a.rs` -- where it names nothing.
fn starts_component(before: Option<char>) -> bool {
    match before {
        None => true,
        Some(ch) => !(ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | '\\')),
    }
}

/// Whether the character *after* a match lets it end a directory.
///
/// The set is closed and small on purpose. Almost every byte is legal in a file
/// name, so asking "can this continue a path?" gets `My Project` wrong against
/// `My Project 2`. Asking instead "is this one of the few characters cargo and
/// rustc actually put after a path?" is the answerable question, and its failure
/// mode is a missed substitution rather than a mangled one.
fn ends_component(after: Option<char>) -> bool {
    match after {
        // The path ends the line.
        None => true,
        // A separator, the `:` before a line number, or the punctuation cargo
        // and rustc wrap paths in.
        Some(ch) => matches!(ch, '/' | '\\' | ':' | '`' | '"' | '\'' | ')' | ',' | ';'),
    }
}

/// The placeholder for a dependency, `serde-json` becoming `$SERDE_JSON`.
///
/// Matches `trybuild`'s spelling so its goldens migrate unedited, including its
/// collision: `a-b` and `a_b` produce the same placeholder. Declaring both is
/// vanishingly rare and the result is still portable, just ambiguous, which is
/// not worth diverging from the spelling a migrating golden already contains.
fn placeholder(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    out.push('$');
    for ch in name.chars() {
        out.push(if ch == '-' {
            '_'
        } else {
            ch.to_ascii_uppercase()
        });
    }
    out
}

/// The path a span header points at, if this line is one.
fn span_target(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    // `::: ` heads the secondary spans rustc prints for a related location.
    trimmed
        .strip_prefix("--> ")
        .or_else(|| trimmed.strip_prefix("::: "))
}

/// Whether a span target names the fixture rather than some other file.
fn points_into(target: &str, fixture: &str) -> bool {
    match target.strip_prefix(fixture) {
        Some(rest) => rest.is_empty() || rest.starts_with(':'),
        None => false,
    }
}

/// `a.rs:22:29` -> `a.rs`, dropping at most a line and a column.
fn hide_trailing_numbers(line: &mut String) {
    for _ in 0..2 {
        let digits = line.bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !line[..line.len() - digits].ends_with(':') {
            return;
        }
        line.truncate(line.len() - digits - 1);
    }
}

/// Overwrite a snippet row's line number with spaces, keeping the gutter width
/// so the `|` column and the carets beneath it stay aligned.
fn blank_leading_number(line: &mut String) {
    // The run is spaces and digits by construction, so blanking it is the
    // identity when it holds no digits and needs no guard.
    let digits = line
        .bytes()
        .take_while(|b| *b == b' ' || b.is_ascii_digit())
        .count();
    line.replace_range(..digits, &" ".repeat(digits));
}

/// Whether a line belongs to the snippet under a span header.
///
/// A snippet row is a numbered source line, a bare `|` gutter row, or the `...`
/// rustc prints where it elided lines. Anything else ends the snippet.
fn is_snippet_row(line: &str) -> bool {
    matches!(
        line.trim_start().chars().next(),
        Some('0'..='9' | '|' | '.')
    )
}

/// Rewrite the target of a span header, and only there.
///
/// Built on the same two helpers the line-number rule uses, so the two can never
/// disagree about what a span header is or which file it names. They did once:
/// this matched `--> ` anywhere on the line, so a fixture whose own source
/// quoted that text had the quote rewritten, and it ignored `::: ` entirely, so
/// a secondary span back into the fixture was treated as foreign and stripped.
fn rewrite_span_target(line: &str, from: &str, to: &str) -> String {
    match span_target(line) {
        Some(target) if points_into(target, from) => {
            let head = &line[..line.len() - target.len()];
            format!("{head}{to}{}", &target[from.len()..])
        }
        _ => line.to_string(),
    }
}

/// Rewrite a toolchain source path to [`RUST`].
///
/// Three shapes reach a diagnostic: a rustup toolchain, whose path carries both
/// the user's home directory *and* the host triple; the older `src/rust/src`
/// layout; and the `/rustc/<commit>/library` form a distributed toolchain
/// reports. Any trait bound involving a std type produces one, which makes this
/// the most common way a golden stops being portable.
fn replace_sysroot(line: &str) -> String {
    const MARKERS: [&str; 2] = [
        "/lib/rustlib/src/rust/library/",
        "/lib/rustlib/src/rust/src/",
    ];

    let mut out = line.to_string();
    for marker in MARKERS {
        // The replacement contains no marker, so each pass strictly shrinks the
        // remaining matches and the loop terminates.
        while let Some(at) = out.find(marker) {
            let start = path_start(&out, at);
            out.replace_range(start..at + marker.len(), &format!("{RUST}/"));
        }
    }
    replace_rustc_commit(&out)
}

/// Where the path containing byte `at` begins.
///
/// The sysroot prefix is machine-specific all the way back to the root, so the
/// whole of it has to go, and the only way to find its start in a line of prose
/// is to walk back to something that cannot be inside a path.
fn path_start(line: &str, at: usize) -> usize {
    line[..at]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | '(' | '<' | '='))
        .map_or(0, |(index, ch)| index + ch.len_utf8())
}

/// Rewrite `/rustc/<40 hex>/library/` to `$RUST/`.
///
/// Self-delimiting, unlike the sysroot markers: the path starts at `/rustc/`, so
/// there is nothing to walk back over.
fn replace_rustc_commit(line: &str) -> String {
    const PREFIX: &str = "/rustc/";
    const SUFFIX: &str = "/library/";
    const COMMIT_LEN: usize = 40;

    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let commit = after
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count()
            .min(after.len());

        if commit == COMMIT_LEN && after[commit..].starts_with(SUFFIX) {
            out.push_str(&rest[..at]);
            out.push_str(RUST);
            out.push('/');
            rest = &after[commit + SUFFIX.len()..];
        } else {
            // Not a commit directory. Step past the prefix so the scan advances.
            out.push_str(&rest[..at + PREFIX.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
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
            dependencies: Vec::new(),
        }
    }

    fn with_deps(deps: &[(&str, &str)]) -> Normalizer {
        let mut n = normalizer();
        n.dependencies = deps
            .iter()
            .map(|(path, name)| ((*path).to_string(), placeholder(name)))
            .collect();
        n.dependencies
            .sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        n
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
        assert_eq!(out, " --> $DIR/dep/src/main.rs\n");
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
            &[],
        );
        assert_eq!(n.scratch_root, "/s");
        assert_eq!(n.scratch_main, "/s/project/src/main.rs");
        assert_eq!(n.manifest_dir, "/h");
    }

    #[test]
    fn a_dependency_outside_the_host_crate_gets_a_placeholder() {
        // The case a golden cannot survive without: a sibling checkout, whose
        // absolute path differs on every machine and in every worktree.
        let n = with_deps(&[("/elsewhere/core", "my-core")]);
        let out = n.normalize(" --> /elsewhere/core/src/event.rs:524:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $MY_CORE/src/event.rs\n");
    }

    #[test]
    fn dependency_placeholders_uppercase_and_underscore_the_name() {
        assert_eq!(placeholder("my-core"), "$MY_CORE");
        assert_eq!(placeholder("core"), "$CORE");
    }

    #[test]
    fn a_dependency_inside_the_host_crate_stays_under_dir() {
        // `$DIR` already says everything portable there is to say, and it is
        // what a migrating `trybuild` golden will contain.
        let n = with_deps(&[("/w/sub", "sub")]);
        let out = n.normalize(" --> /w/sub/src/lib.rs:3:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/sub/src/lib.rs\n");
    }

    #[test]
    fn a_nested_dependency_wins_over_the_one_containing_it() {
        let n = with_deps(&[("/deps", "outer"), ("/deps/inner", "inner")]);
        let out = n.normalize(" --> /deps/inner/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $INNER/src/lib.rs\n");
    }

    #[test]
    fn a_secondary_span_is_treated_like_a_primary_one() {
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let out = n.normalize(" ::: /elsewhere/core/src/lib.rs:9:5\n", "tests/ui/a.rs");
        assert_eq!(out, " ::: $CORE/src/lib.rs\n");
    }

    #[test]
    fn a_foreign_snippet_loses_its_gutter_line_numbers() {
        // Without this the golden still pins the dependency's line numbers, one
        // gutter row down from the span header that was just cleaned. Adding a
        // line anywhere above would re-bless the golden.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0277]: the trait bound is not satisfied\n",
            "   --> /elsewhere/core/src/lib.rs:524:1\n",
            "    |\n",
            "524 | pub struct Event;\n",
            "    | ^^^^^^^^^^^^^^^^\n",
            "    = note: required by this bound\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0277]: the trait bound is not satisfied\n",
                "   --> $CORE/src/lib.rs\n",
                "    |\n",
                "    | pub struct Event;\n",
                "    | ^^^^^^^^^^^^^^^^\n",
                "    = note: required by this bound\n",
            )
        );
    }

    #[test]
    fn the_fixtures_own_snippet_keeps_its_gutter_line_numbers() {
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            " --> src/main.rs:4:17\n",
            "  |\n",
            "4 |     let _x: u8 = \"s\";\n",
            "  |             --   ^^^ expected `u8`\n",
        );
        assert_eq!(
            normalizer().normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:4:17\n",
                "  |\n",
                "4 |     let _x: u8 = \"s\";\n",
                "  |             --   ^^^ expected `u8`\n",
            )
        );
    }

    #[test]
    fn an_elision_marker_does_not_end_a_foreign_snippet() {
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            " --> /elsewhere/core/src/lib.rs:10:1\n",
            "10 | fn a() {}\n",
            "...\n",
            "90 | fn b() {}\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " --> $CORE/src/lib.rs\n",
                "   | fn a() {}\n",
                "...\n",
                "   | fn b() {}\n",
            )
        );
    }

    #[test]
    fn blanking_a_gutter_stops_at_the_next_diagnostic() {
        // The run must not eat the following error's own numbered snippet.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            " --> /elsewhere/core/src/lib.rs:5:1\n",
            "5 | struct A;\n",
            "error[E0308]: mismatched types\n",
            " --> src/main.rs:7:1\n",
            "7 | let _x: u8 = \"s\";\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " --> $CORE/src/lib.rs\n",
                "  | struct A;\n",
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:7:1\n",
                "7 | let _x: u8 = \"s\";\n",
            )
        );
    }

    #[test]
    fn a_sibling_directory_sharing_a_name_prefix_is_left_alone() {
        // `/w` must not claim `/w-helper`. This is the shape a sibling checkout
        // takes, and a plain `str::replace` gets it wrong.
        let out = normalizer().normalize(" --> /w-helper/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /w-helper/src/lib.rs\n");
    }

    #[test]
    fn replace_dir_anchors_on_a_component_boundary() {
        assert_eq!(replace_dir("/w/a.rs:1", "/w", "$DIR"), "$DIR/a.rs:1");
        assert_eq!(replace_dir("/w-dep/a.rs", "/w", "$DIR"), "/w-dep/a.rs");
        assert_eq!(replace_dir("in /w", "/w", "$DIR"), "in $DIR");
        assert_eq!(replace_dir("`/w`", "/w", "$DIR"), "`$DIR`");
        assert_eq!(replace_dir("/wide/a.rs", "/w", "$DIR"), "/wide/a.rs");
        // Both occurrences, and neither of them the sibling.
        assert_eq!(
            replace_dir("/w/a.rs and /w-dep/b.rs and /w/c.rs", "/w", "$DIR"),
            "$DIR/a.rs and /w-dep/b.rs and $DIR/c.rs"
        );
    }

    #[test]
    fn rewrites_a_rustup_toolchain_path_to_rust() {
        // Carries both the user's home directory and the host triple, so a
        // golden holding one is pinned to a single machine. Any trait bound
        // involving a std type produces this.
        let out = normalizer().normalize(
            "    --> /Users/u/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/vec/mod.rs:3938:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, "    --> $RUST/alloc/src/vec/mod.rs\n");
    }

    #[test]
    fn rewrites_the_older_toolchain_source_layout_to_rust() {
        let out = normalizer().normalize(
            " --> /home/u/.rustup/toolchains/nightly/lib/rustlib/src/rust/src/libstd/net/ip.rs:83:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, " --> $RUST/libstd/net/ip.rs\n");
    }

    #[test]
    fn rewrites_a_distributed_toolchain_commit_path_to_rust() {
        let out = normalizer().normalize(
            " --> /rustc/0123456789abcdef0123456789abcdef01234567/library/core/src/mod.rs:9:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/mod.rs\n");
    }

    #[test]
    fn a_path_that_only_looks_like_a_commit_directory_is_left_alone() {
        let out = normalizer().normalize(" --> /rustc/short/library/a.rs:1:1\n", "f.rs");
        assert_eq!(out, " --> /rustc/short/library/a.rs\n");
    }

    #[test]
    fn a_secondary_span_back_into_the_fixture_keeps_its_position() {
        // The bare relative form is how cargo prints files of the package under
        // compilation. Missing it here left the golden naming `src/main.rs`, a
        // file that exists in no user's repository, and stripped the one line
        // number the rule exists to keep.
        let rendered = concat!(
            " ::: src/main.rs:2:5\n",
            "  |\n",
            "2 |     core::bad!(\"s\");\n",
        );
        assert_eq!(
            normalizer().normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " ::: tests/ui/a.rs:2:5\n",
                "  |\n",
                "2 |     core::bad!(\"s\");\n",
            )
        );
    }

    #[test]
    fn a_span_header_quoted_in_the_fixtures_own_source_is_left_alone() {
        // The snippet is the code under test. Rewriting inside it makes the
        // golden misquote the fixture, and churn when the fixture is renamed.
        let out = normalizer().normalize(
            "2 |     let _s = \"--> src/main.rs:1:1\";\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, "2 |     let _s = \"--> src/main.rs:1:1\";\n");
    }

    #[test]
    fn a_sibling_whose_name_extends_this_one_past_a_space_is_left_alone() {
        // Spaces are ordinary in directory names, and `My Project 2` is not a
        // path inside `My Project`.
        let n = with_deps(&[("/x/My Project", "proj")]);
        let out = n.normalize(" --> /x/My Project 2/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /x/My Project 2/src/lib.rs\n");
        // The dependency itself still normalizes.
        let out = n.normalize(" --> /x/My Project/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $PROJ/src/lib.rs\n");
    }

    #[test]
    fn a_match_inside_a_longer_path_is_left_alone() {
        // `/w` names nothing in `/opt/w/a.rs`.
        let out = normalizer().normalize(" --> /opt/w/a.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /opt/w/a.rs\n");
    }

    #[test]
    fn replace_dir_anchors_on_both_sides() {
        assert_eq!(replace_dir("/opt/w/a.rs", "/w", "$DIR"), "/opt/w/a.rs");
        assert_eq!(replace_dir("/w/w/a.rs", "/w", "$DIR"), "$DIR/w/a.rs");
        assert_eq!(replace_dir("/w x/a.rs", "/w", "$DIR"), "/w x/a.rs");
        // An empty needle would match forever without consuming.
        assert_eq!(replace_dir("/w/a.rs", "", "$DIR"), "/w/a.rs");
    }

    #[test]
    fn a_span_target_that_only_shares_a_prefix_with_the_fixture_is_foreign() {
        // `tests/ui/a.rs` must not claim `tests/ui/a.rs.bak`.
        let out = normalizer().normalize(" --> /w/tests/ui/a.rs.bak:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/tests/ui/a.rs.bak\n");
    }
}
