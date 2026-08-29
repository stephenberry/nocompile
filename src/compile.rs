//! Invoking cargo, and sorting what comes back.
//!
//! Every fixture is a bin target of one scratch project, and one invocation
//! builds them all. That is not only fewer process launches: the fixtures are
//! independent crates, so cargo compiles them in parallel, which a
//! fixture-at-a-time loop cannot do at all.
//!
//! Parallel compilation interleaves diagnostics, so the output has to say which
//! target each one came from. `--message-format=json` does; plain stderr does
//! not. That is the whole reason this crate parses JSON, and it pays for itself
//! twice: the attribution is exact rather than inferred, and cargo's own status
//! and summary lines never enter the stream that becomes a golden.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::json;
use crate::scratch::Layout;

/// What one `cargo build` produced, sorted by bin target.
pub(crate) struct Build {
    /// Rendered diagnostics, keyed by bin name, in the order cargo emitted them.
    messages: HashMap<String, Vec<String>>,
    /// Bin names cargo produced an artifact for, which is the only positive
    /// evidence that a target compiled. Absence of errors is not: a target that
    /// was never built also has none.
    compiled: HashSet<String>,
    /// Rendered diagnostics from packages that are *not* the scratch project --
    /// a path dependency that failed to build. Not attributable to any fixture,
    /// but the only description of why every fixture produced nothing.
    foreign: Vec<String>,
    /// Cargo's own stderr. Only read when the build never started.
    pub(crate) stderr: String,
    /// Whether cargo got as far as building anything. False means cargo failed
    /// on its own terms -- an unparseable manifest, an unresolvable dependency
    /// -- and nothing in this struct describes a fixture.
    pub(crate) started: bool,
}

impl Build {
    /// The diagnostics for one bin, joined as they would have been rendered.
    pub(crate) fn diagnostics(&self, bin: &str) -> String {
        match self.messages.get(bin) {
            Some(messages) => messages.concat(),
            None => String::new(),
        }
    }

    pub(crate) fn compiled(&self, bin: &str) -> bool {
        self.compiled.contains(bin)
    }

    /// Why nothing was built, when nothing was built.
    ///
    /// A dependency that fails to compile leaves every fixture with no
    /// diagnostics and no artifact, which on its own reads as a harness bug. The
    /// dependency's own errors say what actually happened, so they are kept
    /// aside rather than discarded for belonging to another package.
    pub(crate) fn nothing_built(&self) -> Option<String> {
        if !self.messages.is_empty() || !self.compiled.is_empty() {
            return None;
        }
        let mut report = self.foreign.concat();
        if report.trim().is_empty() {
            report = self.stderr.clone();
        }
        let report = report.trim_end().to_string();
        (!report.is_empty()).then_some(report)
    }
}

/// Build every bin target of the scratch project in one invocation.
pub(crate) fn build(layout: &Layout) -> io::Result<Build> {
    let mut command = Command::new(cargo());

    command
        .arg("build")
        // Every fixture, not just the first. Diagnostics are attributed by
        // target, so one invocation is enough.
        .arg("--bins")
        // Without this cargo stops scheduling work after the first target
        // fails, so fixtures past the parallelism width would never be compiled
        // at all -- and a target that was never built produces no diagnostics,
        // which is indistinguishable from one that compiled cleanly.
        .arg("--keep-going")
        // The whole point. Diagnostics arrive on stdout tagged with their
        // target; cargo's status and summary lines stay out of the way.
        .arg("--message-format=json")
        // Drops the `Compiling`/`Finished` status lines from stderr.
        .arg("--quiet")
        // stderr is not a TTY here, but cargo can still be configured to force
        // colour, and ANSI escapes in a golden are unreadable and
        // machine-specific.
        .arg("--color=never")
        // A compile-fail suite must never reach the network. A test that
        // silently downloads is a test that fails in CI for an unrelated reason.
        .arg("--offline")
        // Not the outer target directory. See the hazards in `scratch`.
        .arg("--target-dir")
        .arg(&layout.target)
        // Explicit, so the scratch project cannot be resolved against the wrong
        // workspace.
        .arg("--manifest-path")
        .arg(layout.manifest())
        .current_dir(&layout.project);

    // An inherited `-D warnings` turns every fixture's warnings into errors and
    // silently changes what the goldens contain.
    //
    // Removing the variables is not enough. Cargo also reads `[build] rustflags`
    // from `.cargo/config.toml` files discovered from its working directory
    // upward -- and the scratch project lives inside the host crate's target
    // directory, so the host repo's own config applies. Setting
    // `CARGO_ENCODED_RUSTFLAGS` to the empty string is what actually overrides
    // that: it sits at the top of cargo's precedence order and an empty value
    // means "no flags" rather than "unset".
    command
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        // `--target-dir` above already wins, but leaving this set makes the
        // effective target directory ambiguous to anyone reading a failure.
        .env_remove("CARGO_TARGET_DIR");

    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut build = Build {
        messages: HashMap::new(),
        compiled: HashSet::new(),
        foreign: Vec::new(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        started: false,
    };

    let manifest = layout.manifest();
    for line in stdout.lines() {
        // Cargo forwards anything the compiler or a proc macro writes to stdout
        // into this stream verbatim. A `println!` while debugging a derive is
        // routine, and it is not cargo's JSON: skip it rather than fail the run
        // over output that has nothing to do with the fixtures.
        if !line.starts_with('{') {
            continue;
        }
        // A line that opens like a cargo message but will not parse is a
        // different matter, and not something to guess about: the alternative to
        // failing here is a silently short golden.
        let message = json::parse(line).map_err(|error| {
            io::Error::other(format!("could not parse cargo's JSON output: {error}"))
        })?;
        absorb(&mut build, &message, &manifest);
    }

    Ok(build)
}

/// File one cargo message under the target it belongs to.
///
/// `manifest` is the scratch project's own manifest path, and every record is
/// checked against it. Target names are not a namespace: a declared dependency
/// is free to be called the same thing as a generated bin, and without this a
/// dependency's `compiler-artifact` would stand as proof that a fixture
/// compiled -- passing a `pass` fixture that never built, and reporting a
/// `compile_fail` fixture as having compiled. Anything a proc macro prints to
/// stdout would forge the same evidence.
fn absorb(build: &mut Build, message: &json::Value, manifest: &Path) {
    let Some(reason) = message.path_str(&["reason"]) else {
        return;
    };
    let ours = message
        .path_str(&["manifest_path"])
        .is_some_and(|path| Path::new(path) == manifest);
    let target = message.path_str(&["target", "name"]);

    match reason {
        "compiler-message" => {
            let Some(rendered) = message.path_str(&["message", "rendered"]) else {
                return;
            };
            if !ours {
                // Another package's diagnostic. Kept only to explain a run in
                // which no fixture built at all.
                if message.path_str(&["message", "level"]) == Some("error") {
                    build.foreign.push(rendered.to_string());
                }
                return;
            }
            let Some(target) = target else { return };
            // `failure-note` is the "For more information about this error"
            // footer, which is about the *run* rather than the code. Every other
            // level is the compiler talking about the fixture, and is kept:
            // dropping an unrecognized level would quietly shorten a golden,
            // while keeping one is visible the moment it is blessed.
            if message.path_str(&["message", "level"]) == Some("failure-note") {
                return;
            }
            build
                .messages
                .entry(target.to_string())
                .or_default()
                .push(rendered.to_string());
        }
        "compiler-artifact" => {
            if let (true, Some(target)) = (ours, target) {
                build.compiled.insert(target.to_string());
            }
        }
        // Emitted once the build machinery has run, whether or not compilation
        // succeeded. Its absence is how a cargo-level failure is recognized.
        "build-finished" => build.started = true,
        _ => {}
    }
}

/// The cargo that is running us, so the fixture builds on the same toolchain as
/// the test that asked for it. Cargo sets `CARGO` in a test binary's
/// environment; the bare name is only a fallback.
fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Take the scratch project's lock, blocking until it is free.
///
/// Every fixture in a run is written into the *same* scratch project and built
/// by the same invocation, so two runs sharing one host crate would interleave
/// write-then-build and compile each other's fixtures. That is not a theoretical
/// race: `cargo test` runs `#[test]` functions in parallel threads, so two test
/// functions each calling `nocompile::cases!()` hit it every time, and the
/// symptom is a broken fixture reported as passing.
///
/// The lock is held for the whole of a run and released when the returned file
/// is dropped. It is taken on a file rather than an in-process mutex because the
/// same hazard exists across processes -- `cargo nextest` runs test binaries
/// concurrently, and nothing stops two `cargo test` invocations at once.
pub(crate) fn lock(layout: &Layout) -> io::Result<File> {
    std::fs::create_dir_all(&layout.root)?;
    let file = File::create(layout.root.join(".lock"))?;
    file.lock()?;
    Ok(file)
}

/// Write `contents` to `path` only if it differs, so an unchanged fixture does
/// not churn cargo's mtime-based fingerprint.
pub(crate) fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = "/scratch/Cargo.toml";

    fn empty() -> Build {
        Build {
            messages: HashMap::new(),
            compiled: HashSet::new(),
            foreign: Vec::new(),
            stderr: String::new(),
            started: false,
        }
    }

    /// Feed `build` one cargo message written the way cargo writes it.
    fn feed(build: &mut Build, line: &str) {
        let message = json::parse(line).expect("valid cargo json");
        absorb(build, &message, Path::new(OURS));
    }

    fn message(manifest: &str, target: &str, level: &str, rendered: &str) -> String {
        // The newlines in a rendered diagnostic are `\n` escapes on the wire.
        let rendered = rendered.replace('\n', "\\n");
        format!(
            r#"{{"reason":"compiler-message","manifest_path":"{manifest}","target":{{"name":"{target}"}},"message":{{"level":"{level}","rendered":"{rendered}"}}}}"#
        )
    }

    fn artifact(manifest: &str, target: &str) -> String {
        format!(
            r#"{{"reason":"compiler-artifact","manifest_path":"{manifest}","target":{{"name":"{target}"}}}}"#
        )
    }

    #[test]
    fn files_a_diagnostic_under_its_own_target() {
        let mut build = empty();
        feed(&mut build, &message(OURS, "f_a", "error", "error: one\n"));
        feed(&mut build, &message(OURS, "f_b", "error", "error: two\n"));
        feed(
            &mut build,
            &message(OURS, "f_a", "warning", "warning: three\n"),
        );

        assert_eq!(build.diagnostics("f_a"), "error: one\nwarning: three\n");
        assert_eq!(build.diagnostics("f_b"), "error: two\n");
        assert_eq!(build.diagnostics("f_missing"), "");
    }

    #[test]
    fn drops_the_explain_footer() {
        // `For more information about this error...` is about the run, not the
        // code, and would otherwise be blessed into every golden.
        let mut build = empty();
        feed(
            &mut build,
            &message(OURS, "f_a", "failure-note", "For more information...\n"),
        );
        assert_eq!(build.diagnostics("f_a"), "");
    }

    #[test]
    fn an_artifact_is_what_proves_a_target_compiled() {
        let mut build = empty();
        feed(&mut build, &artifact(OURS, "f_a"));
        assert!(build.compiled("f_a"));
        // Not merely "produced no errors": a target cargo never reached also
        // produces none.
        assert!(!build.compiled("f_b"));
    }

    /// Target names are not a namespace. A declared dependency is free to be
    /// called the same thing as a generated bin, and its artifact must not stand
    /// as proof that the fixture compiled -- that would pass a `pass` fixture
    /// that never built, and report a `compile_fail` fixture as compiling.
    #[test]
    fn another_packages_artifact_is_not_evidence_about_a_fixture() {
        let mut build = empty();
        feed(&mut build, &artifact("/elsewhere/Cargo.toml", "f_a"));
        assert!(!build.compiled("f_a"));
    }

    /// Cargo forwards anything a proc macro prints to stdout into this stream.
    #[test]
    fn a_forged_artifact_without_our_manifest_is_ignored() {
        let mut build = empty();
        feed(
            &mut build,
            r#"{"reason":"compiler-artifact","target":{"name":"f_a"}}"#,
        );
        assert!(!build.compiled("f_a"));
    }

    #[test]
    fn another_packages_diagnostic_does_not_reach_a_fixture() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        assert_eq!(build.diagnostics("helper"), "");
    }

    /// A dependency that will not build leaves every fixture with nothing. Its
    /// errors are the only description of what actually happened.
    #[test]
    fn a_dependency_failure_is_reported_rather_than_discarded() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        assert_eq!(build.nothing_built().as_deref(), Some("error: dep"));
    }

    #[test]
    fn nothing_built_stays_quiet_when_something_was() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        feed(&mut build, &message(OURS, "f_a", "error", "error: mine\n"));
        assert_eq!(build.nothing_built(), None);

        let mut build = empty();
        feed(&mut build, &artifact(OURS, "f_a"));
        assert_eq!(build.nothing_built(), None);
    }

    #[test]
    fn build_finished_is_what_says_cargo_got_that_far() {
        let mut build = empty();
        assert!(!build.started);
        feed(&mut build, r#"{"reason":"build-finished","success":false}"#);
        assert!(build.started);
    }

    #[test]
    fn unknown_reasons_are_ignored() {
        let mut build = empty();
        feed(&mut build, r#"{"reason":"build-script-executed"}"#);
        feed(&mut build, r#"{"no-reason-at-all":1}"#);
        assert!(build.messages.is_empty() && build.compiled.is_empty());
    }
}
