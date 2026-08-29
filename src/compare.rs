//! Comparison modes, and the toolchain-churn problem.
//!
//! `.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent
//! to golden-matching rendered text and a rewrite does not fix it -- but it can
//! offer a cheaper mode, which is the one axis on which this crate is *better*
//! than what it replaces rather than merely lighter.
//!
//! Dropping the goldens altogether is not on the table. Comparing exit status
//! alone passes a fixture that fails for a typo in the fixture, and comparing
//! error codes alone asserts nothing at all about `compile_error!`, which
//! carries none. Both turn a green suite into no evidence.

use std::fmt::{self, Display, Formatter};

/// How a fixture's diagnostics are compared against its golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Byte-for-byte after normalization. Maximum information, maximum churn.
    ///
    /// The default, because it is what `trybuild` does -- so a migrating golden
    /// matches unedited -- and because its failure mode is the loud one. An
    /// `Exact` suite that needs re-blessing after a toolchain upgrade says so;
    /// a suite that quietly asserts less than you think does not.
    ///
    /// Choose this when the rendering *is* the product: a `#[diagnostic::
    /// on_unimplemented]` message, a `= help:` suggestion you wrote on purpose,
    /// a span you placed deliberately. [`Brief`](Mode::Brief) drops all three.
    #[default]
    Exact,
    /// Compare each diagnostic's code, primary message and location, and nothing
    /// else.
    ///
    /// Drops the source snippet, the underline art, and the `= note:` / `= help:`
    /// lines -- exactly the parts a rustc release reflows. What survives is the
    /// assertion itself, so a fixture that stops failing, or starts failing for a
    /// *different* reason, still fails the test.
    ///
    /// Note this is not "error codes only". The primary message is kept in full,
    /// which is the point: `compile_error!` -- how a macro reports misuse, and so
    /// the most common diagnostic in the suites this crate exists for -- carries
    /// no error code at all. Comparing codes alone would assert nothing about it
    /// and pass every such fixture vacuously.
    ///
    /// **Reach for this whenever goldens are committed and the crate is expected
    /// to build on more than one toolchain**, which is most crates with a CI
    /// matrix. This crate's own UI suite runs in `Brief` for exactly that reason.
    ///
    /// Keeping the message is also what makes a library's *own* error codes
    /// testable. `rustc`'s `E0xxx` registry is closed, so the convention is a
    /// token in the message -- `compile_error!("MYLIB-E001: ...")`. That token is
    /// part of the primary message, so it lands in the golden and is compared.
    ///
    /// The filter is applied to *both* sides of the comparison, so an `Exact`
    /// golden also passes in `Brief` mode. Switching is therefore a one-line
    /// change, and blessing afterwards shrinks the golden to match.
    Brief,
}

impl Display for Mode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Exact => "Exact",
            Mode::Brief => "Brief",
        })
    }
}

/// Reduce `text` to what `mode` compares.
pub(crate) fn filter(text: &str, mode: Mode) -> String {
    match mode {
        Mode::Exact => text.to_string(),
        Mode::Brief => brief(text),
    }
}

fn brief(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        // A span header. The gutter width in front of it tracks the largest line
        // number in the snippet, so it is trimmed: a fixture growing past line 9
        // must not churn its golden.
        if let Some(span) = trimmed.strip_prefix("--> ") {
            out.push_str("--> ");
            out.push_str(span.trim());
            out.push('\n');
            continue;
        }
        // A primary message: the level, an optional error code, and the text.
        // Column 0 only -- an indented `error:` is inside a snippet.
        if !line.starts_with([' ', '\t'])
            && (line.starts_with("error") || line.starts_with("warning"))
        {
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDERED: &str = "\
error[E0308]: mismatched types
  --> tests/ui/a.rs:4:17
   |
4  |     let x: u8 = \"s\";
   |            --   ^^^ expected `u8`, found `&str`
   |            |
   |            expected due to this
   |
   = note: this error originates in the macro `m`
help: try this
   |
4  |     let x: u8 = 0;
   |

error: aborting due to 1 previous error
";

    #[test]
    fn exact_is_the_identity() {
        assert_eq!(filter(RENDERED, Mode::Exact), RENDERED);
    }

    #[test]
    fn brief_keeps_messages_and_spans_only() {
        assert_eq!(
            filter(RENDERED, Mode::Brief),
            "error[E0308]: mismatched types\n--> tests/ui/a.rs:4:17\nerror: aborting due to 1 previous error\n"
        );
    }

    #[test]
    fn brief_is_idempotent_so_filtering_both_sides_is_safe() {
        let once = filter(RENDERED, Mode::Brief);
        assert_eq!(filter(&once, Mode::Brief), once);
    }

    #[test]
    fn brief_ignores_gutter_width() {
        let narrow = filter("error: x\n --> a.rs:4:1\n", Mode::Brief);
        let wide = filter("error: x\n     --> a.rs:4:1\n", Mode::Brief);
        assert_eq!(narrow, wide);
    }

    #[test]
    fn brief_keeps_warnings() {
        assert_eq!(
            filter(
                "warning: unused variable: `x`\n --> a.rs:2:9\n  |\n",
                Mode::Brief
            ),
            "warning: unused variable: `x`\n--> a.rs:2:9\n"
        );
    }

    #[test]
    fn brief_drops_indented_error_text_inside_a_snippet() {
        assert_eq!(filter("   error: not a header\n", Mode::Brief), "");
    }

    #[test]
    fn mode_default_is_exact() {
        assert_eq!(Mode::default(), Mode::Exact);
    }
}
