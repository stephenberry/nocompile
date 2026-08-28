//! Invoking cargo.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::scratch::Layout;

/// What one `cargo build` produced.
pub(crate) struct Build {
    /// Whether the fixture compiled.
    pub(crate) success: bool,
    /// Cargo's stderr, verbatim.
    pub(crate) stderr: String,
}

/// Build the scratch project's single bin target.
pub(crate) fn build(layout: &Layout) -> io::Result<Build> {
    let mut command = Command::new(cargo());

    command
        .arg("build")
        // Drops the `Compiling`/`Finished` status lines, which is most of the
        // filtering problem.
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

    Ok(Build {
        success: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The cargo that is running us, so the fixture builds on the same toolchain as
/// the test that asked for it. Cargo sets `CARGO` in a test binary's
/// environment; the bare name is only a fallback.
fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Take the scratch project's lock, blocking until it is free.
///
/// Every fixture in a run is written to the *same* `src/main.rs` and built by
/// the same scratch project, so two runs sharing one host crate would interleave
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
