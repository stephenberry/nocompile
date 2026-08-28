//! What a run produced: one [`CaseOutcome`] per fixture, plus any setup failure
//! that prevented the run from starting.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use crate::compare::Mode;
use crate::diff;

/// What a fixture is asserted to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The fixture must not compile, and its diagnostics must match its golden.
    CompileFail,
    /// The fixture must compile. There is no golden; the assertion is the exit status.
    Pass,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::CompileFail => "compile_fail",
            Kind::Pass => "pass",
        }
    }
}

/// Why a single fixture, or the run as a whole, did not hold up.
///
/// This enum is what makes the harness testable by itself (§5.3 of the design):
/// [`TestCases::run`] hands back structured failures instead of panicking, so a
/// test can assert that a bad fixture *fails*, and fails for the stated reason.
///
/// [`TestCases::run`]: crate::TestCases::run
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Failure {
    /// A `compile_fail` fixture compiled. The most important failure the harness
    /// reports, and the one a diff would obscure.
    Compiled,
    /// A `pass` fixture did not compile.
    DidNotCompile {
        /// The normalized diagnostics, for the report.
        stderr: String,
    },
    /// A `compile_fail` fixture failed to build but produced no diagnostics the
    /// harness could attribute to it. Blessing this would write an empty golden
    /// and make the fixture permanently, silently useless.
    NoDiagnostics {
        /// Raw stderr, so the cause is visible.
        stderr: String,
    },
    /// The golden does not exist. A missing golden is a failure, never an
    /// implicit bless -- otherwise a new fixture "passes" on the run that
    /// creates it and nobody reads what it captured.
    MissingGolden {
        /// Path the golden was expected at.
        golden: PathBuf,
    },
    /// The diagnostics do not match the golden.
    Mismatch {
        /// Path of the golden that was compared against.
        golden: PathBuf,
        /// Golden content, after mode filtering.
        expected: String,
        /// Fixture diagnostics, after normalization and mode filtering.
        actual: String,
        /// The comparison mode in force.
        mode: Mode,
    },
    /// A line at column 0 that starts no recognized diagnostic block. Reported
    /// loudly rather than guessed at: the failure mode of a silent filter is
    /// garbage creeping into goldens, and this is the first thing to look at
    /// when a new cargo release changes its output.
    Unclassified {
        /// The offending line.
        line: String,
        /// The full stderr it came from.
        stderr: String,
    },
    /// A fixture directory that matched no `.rs` files. Reported rather than
    /// passed silently: an empty directory means the suite is not running.
    NoFixtures {
        /// The directory that matched nothing.
        directory: PathBuf,
    },
    /// No fixtures were registered at all. The same hazard as [`Failure::NoFixtures`]:
    /// a suite that asserts nothing must not report success.
    NothingRegistered,
    /// Cargo itself failed -- a manifest it could not parse, a dependency it
    /// could not resolve. Not a property of the fixture.
    Cargo {
        /// Cargo's own message.
        message: String,
    },
    /// The harness could not read or write a file.
    Io {
        /// What it was trying to do.
        context: String,
        /// The underlying `io::Error`, rendered. Kept as text so a failure can
        /// be cloned and reported by more than one run.
        message: String,
    },
}

impl Display for Failure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Compiled => f.write_str("expected a compile error, but the fixture compiled"),
            Failure::DidNotCompile { stderr } => {
                write!(
                    f,
                    "expected the fixture to compile, but it did not:\n\n{stderr}"
                )
            }
            Failure::NoDiagnostics { stderr } => write!(
                f,
                "the fixture failed to build but produced no diagnostics the harness could \
                 attribute to it; refusing to write an empty golden. Raw stderr:\n\n{stderr}"
            ),
            Failure::MissingGolden { golden } => write!(
                f,
                "no golden at {}\nrun with NOBUILD=overwrite to create it, then read what it captured",
                golden.display()
            ),
            Failure::Mismatch {
                golden,
                expected,
                actual,
                mode,
            } => {
                write!(
                    f,
                    "diagnostics do not match {} ({mode} mode)",
                    golden.display()
                )?;
                if let Some(line) = diff::first_difference(expected, actual) {
                    write!(f, ", first difference at line {line}")?;
                }
                let diff = diff::unified(expected, actual, &golden.display().to_string(), "actual");
                write!(
                    f,
                    "\n\n{diff}\nrun with NOBUILD=overwrite to update the golden"
                )
            }
            Failure::Unclassified { line, stderr } => write!(
                f,
                "nobuild does not understand this output line:\n\n    {line}\n\n\
                 It is at column 0 and starts no recognized diagnostic block, so the harness \
                 cannot tell whether it belongs in the golden. Full stderr:\n\n{stderr}"
            ),
            Failure::NoFixtures { directory } => write!(
                f,
                "no .rs fixtures in {} -- the suite would pass without testing anything",
                directory.display()
            ),
            Failure::NothingRegistered => f.write_str(
                "no fixtures were registered -- the suite would pass without testing anything",
            ),
            Failure::Cargo { message } => {
                write!(f, "cargo could not run the fixture build:\n\n{message}")
            }
            Failure::Io { context, message } => write!(f, "{context}: {message}"),
        }
    }
}

impl Error for Failure {}

/// The result of one fixture.
#[derive(Debug)]
pub struct CaseOutcome {
    path: PathBuf,
    kind: Kind,
    result: Result<(), Failure>,
}

impl CaseOutcome {
    pub(crate) fn new(path: PathBuf, kind: Kind, result: Result<(), Failure>) -> Self {
        Self { path, kind, result }
    }

    /// The fixture's path, relative to the host crate's manifest directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the fixture was a `compile_fail` or a `pass` case.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The failure, if this case did not hold up.
    pub fn failure(&self) -> Option<&Failure> {
        self.result.as_ref().err()
    }

    /// Whether this case held up.
    pub fn is_success(&self) -> bool {
        self.result.is_ok()
    }
}

/// Everything one [`TestCases::run`] produced.
///
/// [`TestCases::run`]: crate::TestCases::run
#[derive(Debug)]
pub struct Outcome {
    setup: Vec<Failure>,
    cases: Vec<CaseOutcome>,
}

impl Outcome {
    pub(crate) fn new(setup: Vec<Failure>, cases: Vec<CaseOutcome>) -> Self {
        Self { setup, cases }
    }

    /// Failures that stopped the run before, or independently of, any fixture --
    /// a fixture directory that does not exist, a scratch project that could not
    /// be written.
    pub fn setup_failures(&self) -> &[Failure] {
        &self.setup
    }

    /// Every fixture that ran, in the order it was registered.
    pub fn cases(&self) -> &[CaseOutcome] {
        &self.cases
    }

    /// Just the fixtures that did not hold up.
    pub fn failures(&self) -> impl Iterator<Item = &CaseOutcome> {
        self.cases.iter().filter(|case| !case.is_success())
    }

    /// Whether every fixture held up and setup was clean.
    pub fn is_success(&self) -> bool {
        self.setup.is_empty() && self.cases.iter().all(CaseOutcome::is_success)
    }

    /// A plain-text report. No colour: a test harness that only reads well in
    /// colour reads badly in CI logs, and colour costs a dependency.
    pub fn report(&self) -> String {
        use fmt::Write as _;

        let mut out = String::new();
        let failed = self.failures().count();
        let total = self.cases.len();

        if self.setup.is_empty() && failed == 0 {
            let _ = write!(out, "nobuild: {total} case(s) passed");
            return out;
        }

        let _ = writeln!(out, "nobuild: {failed} of {total} case(s) failed");

        for failure in &self.setup {
            let _ = writeln!(out, "\nSETUP FAILED");
            indent(&mut out, failure);
        }

        for case in self.failures() {
            let Some(failure) = case.failure() else {
                continue;
            };
            let _ = writeln!(
                out,
                "\nFAIL {} ({})",
                case.path().display(),
                case.kind().label()
            );
            indent(&mut out, failure);
        }

        out
    }
}

/// Write a failure under its heading, indented four spaces. `Failure`'s own
/// `Display` deliberately emits unindented text -- a failure rendered on its own
/// should not arrive pre-indented for someone else's layout -- so this is the
/// only place indentation is applied.
fn indent(out: &mut String, failure: &Failure) {
    for line in failure.to_string().lines() {
        if !line.is_empty() {
            out.push_str("    ");
            out.push_str(line);
        }
        out.push('\n');
    }
}
