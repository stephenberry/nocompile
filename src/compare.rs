//! Comparison modes, and the toolchain-churn problem.
//!
//! `.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent
//! to golden-matching rendered text and a rewrite does not fix it -- but it can
//! offer a cheaper mode, which is the one axis on which this crate is *better*
//! than what it replaces rather than merely lighter.

use std::fmt::{self, Display, Formatter};

/// How a fixture's diagnostics are compared against its golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Byte-for-byte after normalization. Maximum information, maximum churn.
    ///
    /// The default, because it is what people expect and it is strictly more
    /// informative.
    #[default]
    Exact,
    /// Compare only error codes, primary messages and span headers.
    ///
    /// Drops the source snippet, the underline art, and the `= note:` / `= help:`
    /// lines -- exactly the parts a rustc release reflows. Still catches every
    /// regression that matters: a fixture that stops failing, or starts failing
    /// for a *different* reason.
    ///
    /// The filter is applied to *both* sides of the comparison, so an `Exact`
    /// golden also passes in `Codes` mode. Switching to `Codes` is therefore a
    /// one-line change, and blessing afterwards shrinks the golden to match.
    Codes,
}

impl Display for Mode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Exact => "Exact",
            Mode::Codes => "Codes",
        })
    }
}

/// Reduce `text` to what `mode` compares.
pub(crate) fn filter(text: &str, mode: Mode) -> String {
    match mode {
        Mode::Exact => text.to_string(),
        Mode::Codes => codes(text),
    }
}

fn codes(text: &str) -> String {
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
    fn codes_keeps_messages_and_spans_only() {
        assert_eq!(
            filter(RENDERED, Mode::Codes),
            "error[E0308]: mismatched types\n--> tests/ui/a.rs:4:17\nerror: aborting due to 1 previous error\n"
        );
    }

    #[test]
    fn codes_is_idempotent_so_filtering_both_sides_is_safe() {
        let once = filter(RENDERED, Mode::Codes);
        assert_eq!(filter(&once, Mode::Codes), once);
    }

    #[test]
    fn codes_ignores_gutter_width() {
        let narrow = filter("error: x\n --> a.rs:4:1\n", Mode::Codes);
        let wide = filter("error: x\n     --> a.rs:4:1\n", Mode::Codes);
        assert_eq!(narrow, wide);
    }

    #[test]
    fn codes_keeps_warnings() {
        assert_eq!(
            filter(
                "warning: unused variable: `x`\n --> a.rs:2:9\n  |\n",
                Mode::Codes
            ),
            "warning: unused variable: `x`\n--> a.rs:2:9\n"
        );
    }

    #[test]
    fn codes_drops_indented_error_text_inside_a_snippet() {
        assert_eq!(filter("   error: not a header\n", Mode::Codes), "");
    }

    #[test]
    fn mode_default_is_exact() {
        assert_eq!(Mode::default(), Mode::Exact);
    }
}
