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
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        started: false,
    };

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // A line cargo emitted that will not parse is not something to guess
        // about: the alternative to failing here is a silently short golden.
        let message = json::parse(line).map_err(|error| {
            io::Error::other(format!("could not parse cargo's JSON output: {error}"))
        })?;
        absorb(&mut build, &message);
    }

    Ok(build)
}

/// File one cargo message under the target it belongs to.
fn absorb(build: &mut Build, message: &json::Value) {
    let Some(reason) = message.path_str(&["reason"]) else {
        return;
    };
    let target = message.path_str(&["target", "name"]);

    match reason {
        "compiler-message" => {
            let Some(target) = target else { return };
            let Some(rendered) = message.path_str(&["message", "rendered"]) else {
                return;
            };
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
            if let Some(target) = target {
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
