//! `nocompile`'s own UI suite, run by `nocompile`.
//!
//! These fixtures assert language invariants rather than anything specific to
//! this crate, because the crate exposes no macros or traits of its own to
//! misuse. What they do exercise is the harness end to end against committed
//! goldens and real rustc diagnostics -- which the self-test suite, whose
//! fixtures are all written at run time, does not.
//!
//! The suite runs in [`Mode::Brief`] on purpose: the goldens here are committed
//! and this crate is expected to build on more than one toolchain, which is the
//! exact situation `Brief` exists for.

use nocompile::Mode;

#[test]
fn ui() {
    let mut t = nocompile::cases!();
    t.mode(Mode::Brief);
    t.compile_fail_dir("tests/ui");
    t.pass_dir("tests/ui-pass");
    t.assert();
}
