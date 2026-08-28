//! The harness, tested by itself.
//!
//! A test harness cannot be trusted to test itself naively: one that reports
//! success unconditionally passes its own suite. So this suite is adversarial.
//! Of the five cases the design calls for, four assert that the harness
//! **fails**, and each asserts *which* failure -- which is why `TestCases::run`
//! returns an `Outcome` instead of panicking, and why `Failure` is a structured
//! enum rather than a string.
//!
//! Fixtures are written at run time into a sandbox under the target directory
//! rather than committed, because two of these cases have to corrupt or delete a
//! golden.

use std::fs;
use std::path::{Path, PathBuf};

use nobuild::{Failure, Mode, Outcome, TestCases};

/// A throwaway host crate: a directory of fixtures the test owns outright.
struct Sandbox {
    name: String,
    dir: PathBuf,
}

impl Sandbox {
    /// `name` must be unique per test: it names both the sandbox directory and
    /// the harness's scratch project, and `cargo test` runs tests in parallel.
    fn new(name: &str) -> Self {
        let dir = target_dir().join("nobuild-selftest").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create sandbox");
        Sandbox {
            name: name.to_string(),
            dir,
        }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.dir.join(relative);
        fs::create_dir_all(path.parent().expect("fixture has a parent"))
            .expect("create fixture dir");
        fs::write(&path, contents).expect("write fixture");
        path
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).expect("read sandbox file")
    }

    fn cases(&self) -> TestCases {
        TestCases::new(&self.dir, &self.name)
    }
}

/// The target directory this test binary was built into.
fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("target"),
    }
}

/// A fixture that cannot compile, for a reason rustc has spelled the same way
/// for many releases.
const REJECTED: &str = "fn main() {\n    let _x: u8 = \"not a u8\";\n}\n";

/// A fixture that compiles cleanly, with no warnings to leak into a golden.
const ACCEPTED: &str = "fn main() {\n    let _x: u8 = 0;\n    println!(\"{_x}\");\n}\n";

/// The single failure of a one-case run.
#[track_caller]
fn sole_failure(outcome: &Outcome) -> &Failure {
    assert!(
        outcome.setup_failures().is_empty(),
        "unexpected setup failure:\n{}",
        outcome.report()
    );
    assert_eq!(outcome.cases().len(), 1, "expected exactly one case");
    outcome.cases()[0]
        .failure()
        .unwrap_or_else(|| panic!("expected the case to fail, but it passed"))
}

#[track_caller]
fn assert_passed(outcome: &Outcome) {
    assert!(
        outcome.is_success(),
        "expected a pass:\n{}",
        outcome.report()
    );
}

// ---------------------------------------------------------------------------
// The five cases of §5.3.
// ---------------------------------------------------------------------------

/// 1. A fixture that fails to compile, with a correct golden, passes.
#[test]
fn a_rejected_fixture_with_a_correct_golden_passes() {
    let sandbox = Sandbox::new("correct-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");

    // Bless, then read what it captured -- which is the whole point of blessing
    // being a separate, deliberate step.
    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/rejected.stderr");
    assert!(
        golden.contains("error[E0308]: mismatched types"),
        "{golden}"
    );
    assert!(
        golden.contains("--> ui/rejected.rs:2:18"),
        "the span should point at the fixture, not the scratch project:\n{golden}"
    );
    assert!(
        !golden.contains("nobuild-scratch") && !golden.contains("src/main.rs"),
        "the scratch project leaked into the golden:\n{golden}"
    );

    assert_passed(&t.overwrite(false).run());
}

/// 2. The same fixture with a deliberately wrong golden must fail.
#[test]
fn a_rejected_fixture_with_a_wrong_golden_fails() {
    let sandbox = Sandbox::new("wrong-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());

    let corrupted = sandbox
        .read("ui/rejected.stderr")
        .replace("mismatched types", "some other problem entirely");
    sandbox.write("ui/rejected.stderr", &corrupted);

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    let Failure::Mismatch { golden, mode, .. } = failure else {
        panic!("expected Mismatch, got {failure:?}");
    };
    assert_eq!(golden, Path::new("ui/rejected.stderr"));
    assert_eq!(*mode, Mode::Exact);

    // The report has to name the fixture and show the difference, or nobody can
    // act on it.
    let report = outcome.report();
    assert!(report.contains("ui/rejected.rs"), "{report}");
    assert!(
        report.contains("-error[E0308]: some other problem entirely"),
        "{report}"
    );
    assert!(
        report.contains("+error[E0308]: mismatched types"),
        "{report}"
    );
}

/// 3. A fixture that compiles, declared `compile_fail`, must fail.
#[test]
fn a_fixture_that_compiles_fails_a_compile_fail_case() {
    let sandbox = Sandbox::new("unexpectedly-compiles");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs");

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    assert!(matches!(failure, Failure::Compiled), "{failure:?}");
    assert!(
        outcome.report().contains("but the fixture compiled"),
        "{}",
        outcome.report()
    );
}

/// 4. A fixture with no golden must fail, not bless.
#[test]
fn a_missing_golden_is_a_failure_not_an_implicit_bless() {
    let sandbox = Sandbox::new("missing-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs").overwrite(false);

    let outcome = t.run();
    let failure = sole_failure(&outcome);
    let Failure::MissingGolden { golden } = failure else {
        panic!("expected MissingGolden, got {failure:?}");
    };
    assert_eq!(golden, Path::new("ui/rejected.stderr"));
    assert!(
        !sandbox.path("ui/rejected.stderr").exists(),
        "a failing run must not write the golden it was missing"
    );
}

/// 5. A `pass` fixture that does not compile must fail.
#[test]
fn a_pass_fixture_that_does_not_compile_fails() {
    let sandbox = Sandbox::new("pass-does-not-compile");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.pass("ui/rejected.rs");

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    let Failure::DidNotCompile { stderr } = failure else {
        panic!("expected DidNotCompile, got {failure:?}");
    };
    assert!(stderr.contains("error[E0308]"), "{stderr}");
    assert!(
        stderr.contains("ui/rejected.rs"),
        "the message should point at the fixture:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// The rest of the contract.
// ---------------------------------------------------------------------------

/// The other half of a UI suite: proof that the *allowed* form still compiles.
#[test]
fn a_pass_fixture_that_compiles_passes() {
    let sandbox = Sandbox::new("pass-compiles");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.pass("ui/accepted.rs");
    assert_passed(&t.overwrite(false).run());
}

/// Blessing must never write a golden for a fixture that compiled: there is no
/// stderr to write, and an empty golden makes the fixture permanently and
/// silently useless.
#[test]
fn blessing_refuses_a_fixture_that_compiled() {
    let sandbox = Sandbox::new("bless-refuses");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs");

    let outcome = t.overwrite(true).run();
    assert!(matches!(sole_failure(&outcome), Failure::Compiled));
    assert!(
        !sandbox.path("ui/accepted.stderr").exists(),
        "bless wrote a golden for a fixture that compiled"
    );
}

/// A fixture is copied verbatim. The harness must not guess at adding a
/// `fn main`: detecting one reliably needs a parser, and a wrong guess writes
/// harness-injected source into the golden under the fixture's own name.
#[test]
fn a_fixture_is_never_rewritten_before_compiling() {
    let sandbox = Sandbox::new("verbatim");
    // Contains the substring `fn main` but declares no `main`. A substring test
    // would take this for a real one; a naive appender would inject a `fn main`
    // this fixture's line numbers do not account for.
    sandbox.write(
        "ui/helper.rs",
        "fn main_helper() -> u8 {\n    0\n}\n\nconst _: u8 = \"not a u8\";\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/helper.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/helper.stderr");
    assert!(
        golden.contains("--> ui/helper.rs:5:15"),
        "spans must match the fixture as written:\n{golden}"
    );
    assert!(
        !golden.contains("fn main() {}"),
        "the harness injected source into the golden:\n{golden}"
    );
}

/// A fixture's own diagnostic must never be mistaken for one of cargo's or
/// rustc's summaries. A derive is free to phrase a `compile_error!` any way it
/// likes, and dropping it would delete the invariant under test from its own
/// golden while the harness reported green.
#[test]
fn a_fixture_error_worded_like_a_summary_reaches_the_golden() {
    let sandbox = Sandbox::new("summary-wording");
    sandbox.write(
        "ui/worded.rs",
        "compile_error!(\"could not compile `this input` due to a missing impl\");\n\nfn main() {}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/worded.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/worded.stderr");
    assert!(
        golden.contains("could not compile `this input` due to a missing impl"),
        "the fixture's own diagnostic was dropped as a summary:\n{golden}"
    );

    // And deleting the guarded construct must now be caught rather than pass.
    sandbox.write("ui/worded.rs", "\n\nfn main() {}\n");
    assert!(
        !t.overwrite(false).run().is_success(),
        "removing the guarded construct went unnoticed"
    );
}

/// The same hazard inverted: a fixture whose diagnostic reads like one of
/// cargo's own failures must still be blessable.
#[test]
fn a_fixture_error_worded_like_a_cargo_failure_reaches_the_golden() {
    let sandbox = Sandbox::new("cargo-wording");
    sandbox.write(
        "ui/worded.rs",
        "compile_error!(\"failed to parse the codec attribute\");\n\nfn main() {}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/worded.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/worded.stderr");
    assert!(
        golden.contains("failed to parse the codec attribute"),
        "the fixture's own diagnostic was misreported as a cargo failure:\n{golden}"
    );
}

/// Normalization must not rewrite the fixture's own source text. The snippet
/// quotes the code under test; a substitution inside it misquotes the fixture
/// and misaligns the carets beneath.
#[test]
fn normalization_leaves_quoted_source_alone() {
    let sandbox = Sandbox::new("quoted-source");
    sandbox.write(
        "ui/quotes_a_path.rs",
        "fn main() {\n    let _x: u8 = \"src/main.rs\";\n}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/quotes_a_path.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/quotes_a_path.stderr");
    assert!(
        golden.contains("let _x: u8 = \"src/main.rs\""),
        "the fixture's source was rewritten inside its own snippet:\n{golden}"
    );
    assert!(
        golden.contains("--> ui/quotes_a_path.rs:2:18"),
        "the span was not rewritten:\n{golden}"
    );
}

/// A suite that registers nothing must not report success -- the same hazard
/// `NoFixtures` covers, one level up.
#[test]
fn a_suite_that_registers_nothing_is_reported() {
    let sandbox = Sandbox::new("nothing-registered");
    let outcome = sandbox.cases().run();
    assert!(!outcome.is_success());
    assert!(
        matches!(outcome.setup_failures(), [Failure::NothingRegistered]),
        "{:?}",
        outcome.setup_failures()
    );
}

/// Every fixture in a run is written to the same scratch `src/main.rs`, and
/// `cargo test` runs test functions in parallel threads. Without a lock the two
/// runs below compile each other's fixtures, and the reliable symptom is a
/// broken fixture reported as passing.
#[test]
fn concurrent_runs_do_not_compile_each_others_fixtures() {
    const ROUNDS: usize = 8;

    let sandbox = Sandbox::new("concurrent");
    sandbox.write("broken/fixture.rs", REJECTED);
    sandbox.write("fine/fixture.rs", ACCEPTED);

    std::thread::scope(|scope| {
        let broken = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = sandbox.cases();
                t.pass("broken/fixture.rs").overwrite(false);
                assert!(
                    !t.run().is_success(),
                    "a fixture that cannot compile was reported as passing"
                );
            }
        });
        let fine = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = sandbox.cases();
                t.pass("fine/fixture.rs").overwrite(false);
                assert_passed(&t.run());
            }
        });
        broken.join().expect("broken thread");
        fine.join().expect("fine thread");
    });
}

/// `Codes` filters both sides, so switching modes does not force a re-bless
/// before the suite can go green.
#[test]
fn codes_mode_accepts_an_exact_golden() {
    let sandbox = Sandbox::new("codes-mode");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());
    let exact = sandbox.read("ui/rejected.stderr");

    assert_passed(&t.mode(Mode::Codes).overwrite(false).run());

    // Blessing in `Codes` then shrinks the golden to what it actually compares.
    assert_passed(&t.overwrite(true).run());
    let codes = sandbox.read("ui/rejected.stderr");
    assert!(
        codes.len() < exact.len(),
        "Codes golden was not smaller:\n{codes}"
    );
    assert!(codes.contains("error[E0308]: mismatched types"), "{codes}");
    assert!(codes.contains("--> ui/rejected.rs:2:18"), "{codes}");
    assert!(
        !codes.contains("let _x"),
        "Codes kept the source snippet:\n{codes}"
    );
}

/// `Codes` must still catch a fixture that starts failing for a different
/// reason -- otherwise it would be trading churn for blindness.
#[test]
fn codes_mode_still_catches_a_changed_error() {
    let sandbox = Sandbox::new("codes-catches");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs").mode(Mode::Codes);
    assert_passed(&t.overwrite(true).run());

    // Same fixture name, different invariant broken.
    sandbox.write(
        "ui/rejected.rs",
        "fn main() {\n    undefined_function();\n}\n",
    );
    let outcome = t.overwrite(false).run();
    assert!(matches!(sole_failure(&outcome), Failure::Mismatch { .. }));
}

/// Directory registration takes every `.rs` file, in file-name order, and pairs
/// each with the golden beside it.
#[test]
fn a_directory_registers_every_fixture_in_order() {
    let sandbox = Sandbox::new("directory");
    sandbox.write("ui/b_second.rs", REJECTED);
    sandbox.write(
        "ui/a_first.rs",
        "fn main() {\n    undefined_function();\n}\n",
    );
    sandbox.write("ui/notes.txt", "not a fixture");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    assert_passed(&outcome);

    let paths: Vec<_> = outcome
        .cases()
        .iter()
        .map(|c| c.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [Path::new("ui/a_first.rs"), Path::new("ui/b_second.rs")]
    );
    assert!(sandbox.path("ui/a_first.stderr").exists());
    assert!(sandbox.path("ui/b_second.stderr").exists());
}

/// A fixture directory that matches nothing means the suite is not running, and
/// saying so beats reporting a clean pass.
#[test]
fn an_empty_fixture_directory_is_reported() {
    let sandbox = Sandbox::new("empty-dir");
    fs::create_dir_all(sandbox.path("ui")).expect("create empty dir");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");

    let outcome = t.run();
    assert!(!outcome.is_success());
    assert!(
        matches!(outcome.setup_failures(), [Failure::NoFixtures { .. }]),
        "{:?}",
        outcome.setup_failures()
    );
}

/// A missing fixture directory is reported rather than panicking at
/// registration time, since registration has no way to return an error.
#[test]
fn a_missing_fixture_directory_is_reported() {
    let sandbox = Sandbox::new("missing-dir");

    let mut t = sandbox.cases();
    t.compile_fail_dir("does-not-exist");

    let outcome = t.run();
    assert!(!outcome.is_success());
    assert!(
        outcome
            .report()
            .contains("could not read the fixture directory does-not-exist"),
        "{}",
        outcome.report()
    );
}

/// The declared-dependency path (D2), end to end: a fixture can use the crate
/// under test, and only the crates the caller named.
#[test]
fn a_declared_path_dependency_reaches_the_fixtures() {
    let sandbox = Sandbox::new("path-dependency");
    sandbox.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    sandbox.write("helper/src/lib.rs", "pub fn small() -> u8 {\n    0\n}\n");
    sandbox.write(
        "ui-pass/uses_helper.rs",
        "fn main() {\n    let _x: u8 = helper::small();\n}\n",
    );
    sandbox.write(
        "ui/misuses_helper.rs",
        "fn main() {\n    let _x: String = helper::small();\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.pass("ui-pass/uses_helper.rs");
    t.compile_fail("ui/misuses_helper.rs");

    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/misuses_helper.stderr");
    assert!(
        golden.contains("error[E0308]: mismatched types"),
        "{golden}"
    );
    assert!(golden.contains("--> ui/misuses_helper.rs:2:22"), "{golden}");
}

/// A run reports every case, not just the first to fail.
#[test]
fn all_cases_are_reported_not_just_the_first_failure() {
    let sandbox = Sandbox::new("reports-all");
    sandbox.write("ui/a.rs", ACCEPTED);
    sandbox.write("ui/b.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui").overwrite(false);

    let outcome = t.run();
    assert_eq!(outcome.failures().count(), 2);
    let report = outcome.report();
    assert!(report.contains("2 of 2 case(s) failed"), "{report}");
    assert!(report.contains("FAIL ui/a.rs (compile_fail)"), "{report}");
    assert!(report.contains("FAIL ui/b.rs (compile_fail)"), "{report}");
}

/// `assert` panics with the report rather than a bare assertion failure.
#[test]
fn assert_panics_with_the_report() {
    let sandbox = Sandbox::new("assert-panics");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs").overwrite(false);

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.assert()));
    let payload = panicked.expect_err("assert should have panicked");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or("<not a string>");
    assert!(message.contains("FAIL ui/accepted.rs"), "{message}");
    assert!(message.contains("but the fixture compiled"), "{message}");
}

/// Cargo failing on its own terms is not a property of the fixture, and must not
/// be mistaken for one. A golden blessed from an unresolvable manifest would
/// record the harness's misconfiguration rather than the invariant under test.
#[test]
fn a_cargo_failure_is_not_reported_as_a_diagnostic() {
    let sandbox = Sandbox::new("cargo-failure");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    t.raw_manifest_lines("this is not valid toml [[[");

    let outcome = t.overwrite(true).run();
    let failure = sole_failure(&outcome);
    let Failure::Cargo { message } = failure else {
        panic!("expected Cargo, got {failure:?}");
    };
    assert!(message.contains("expected `=`"), "{message}");
    assert!(
        !sandbox.path("ui/rejected.stderr").exists(),
        "bless wrote a golden from a manifest cargo could not parse"
    );
}
