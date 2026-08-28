//! Turning cargo's plain stderr into just the fixture's diagnostics.
//!
//! The alternative is `--message-format=json` and a JSON parser, which is what
//! `trybuild` does. It is the robust choice and it costs a dependency. This
//! crate takes the other branch, for the reason recorded in the design: rustc's
//! JSON `rendered` field is byte-for-byte what plain stderr prints, so with
//! `--quiet` the stderr stream is already almost exactly the golden. Writing a
//! JSON parser to avoid a JSON dependency trades a dependency for a maintenance
//! surface, and the cargo message schema is no more stable than its text.
//!
//! What remains is cargo's and rustc's own summary lines, which name the scratch
//! crate and count the errors. Those are *classified* out rather than filtered
//! out: every column-0 line is matched against a known shape, and one that
//! matches nothing is a hard error. The failure mode of a silent filter is
//! garbage creeping into goldens; the failure mode of this is a loud, actionable
//! message the first time cargo changes its output.
//!
//! # Why this works on blocks rather than lines
//!
//! Message text alone cannot tell a summary from a diagnostic. A derive is free
//! to emit ``compile_error!("aborting due to a shape this derive cannot
//! support")`` or `compile_error!("failed to parse the attribute")`, and a
//! line-level prefix match would drop the first and misreport the second as a
//! cargo failure -- deleting the invariant under test from its own golden.
//!
//! The reliable discriminator is the **span**. Cargo's and rustc's summaries
//! count what happened and point at nothing; every diagnostic about a fixture
//! points somewhere with a `-->` line. So the whole block is assembled first,
//! and only a block with no span at all is eligible to be dropped as a summary
//! or reported as a cargo failure. A block that points at the *generated
//! manifest* is a cargo failure for the same reason inverted: the manifest is
//! this crate's own, so an error about it is never the invariant under test.

/// A column-0 line the classifier could not place.
#[derive(Debug)]
pub(crate) enum ClassifyError {
    /// Matched no known shape. Reported to the user verbatim.
    Unrecognized(String),
    /// Cargo failed on its own terms -- a manifest it could not parse, a
    /// dependency it could not resolve. Not a property of the fixture.
    Cargo(String),
}

/// Extract the fixture's diagnostics from a raw stderr stream.
pub(crate) fn classify(stderr: &str) -> Result<String, ClassifyError> {
    let mut kept: Vec<&str> = Vec::new();

    for block in split(stderr)? {
        if block.is_kept()? {
            kept.extend(&block.lines);
        }
    }

    // A dropped block's trailing blank lines can leave the kept text ending in
    // whitespace; the normalizer settles the final newline, so only strip here.
    let mut text = kept.join("\n");
    while text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

/// One diagnostic, from its column-0 header through the last line belonging to
/// it, or one of rustc's closing footers.
struct Block<'a> {
    /// The parsed header, or `None` for a footer, which has no level.
    head: Option<Head<'a>>,
    /// Every line of the block, header included.
    lines: Vec<&'a str>,
    /// The target of the block's first `-->` line, if it has one.
    span: Option<&'a str>,
}

impl<'a> Block<'a> {
    /// Whether this block belongs in the golden.
    fn is_kept(&self) -> Result<bool, ClassifyError> {
        let Some(head) = &self.head else {
            return Ok(false); // A footer.
        };

        // An error against the manifest this crate generates. Cargo renders
        // these in rustc's exact diagnostic style, span and all, so only the
        // span distinguishes them.
        if head.level == "error" && self.span.is_some_and(points_at_manifest) {
            return Err(ClassifyError::Cargo(self.lines.join("\n")));
        }

        // Everything below identifies a block by what it does *not* have. A
        // block with a span describes code, so it is the fixture's and it stays.
        if self.span.is_some() {
            return Ok(true);
        }

        if head.is_summary() {
            return Ok(false);
        }
        if head.is_cargo_error() {
            return Err(ClassifyError::Cargo(self.lines.join("\n")));
        }
        Ok(true)
    }
}

/// Split stderr into blocks, failing on any column-0 line that starts none.
fn split(stderr: &str) -> Result<Vec<Block<'_>>, ClassifyError> {
    let mut blocks: Vec<Block<'_>> = Vec::new();

    for line in stderr.lines() {
        // Cargo's error chain explains the error above it, so it joins that
        // block rather than starting one.
        if is_continuation(line) || (line == "Caused by:" && !blocks.is_empty()) {
            let Some(block) = blocks.last_mut() else {
                // rustc always starts a diagnostic with a column-0 header, so a
                // continuation with no block open cannot be part of one -- it is
                // a cargo status line (`Blocking waiting for file lock ...`),
                // which `--quiet` mostly but not entirely suppresses.
                continue;
            };
            if block.span.is_none() {
                block.span = span_target(line);
            }
            block.lines.push(line);
            continue;
        }

        let head = if is_footer(line) {
            None
        } else {
            Some(Head::parse(line).ok_or_else(|| ClassifyError::Unrecognized(line.to_string()))?)
        };
        blocks.push(Block {
            head,
            lines: vec![line],
            span: None,
        });
    }

    Ok(blocks)
}

/// The path a `-->` line points at, stripped of its `:line:col` suffix.
fn span_target(line: &str) -> Option<&str> {
    let span = line.trim_start().strip_prefix("--> ")?;
    Some(span.split(':').next().unwrap_or(span))
}

/// Whether a span points at a `Cargo.toml`. The only manifest a fixture build
/// can see is the one this crate generates.
fn points_at_manifest(span: &str) -> bool {
    span == "Cargo.toml" || span.ends_with("/Cargo.toml")
}

/// rustc's closing footers, which follow the last diagnostic and summarize the
/// run rather than describing the code:
///
/// ```text
/// For more information about this error, try `rustc --explain E0308`.
/// Some errors have detailed explanations: E0308, E0433.
/// ```
///
/// They carry no span and no new information, and the second one churns whenever
/// a fixture's error set changes, so they are dropped.
fn is_footer(line: &str) -> bool {
    line.starts_with("For more information about ")
        || line.starts_with("Some errors have detailed explanations:")
        || line.starts_with("Some warnings have detailed explanations:")
}

/// Whether a line continues the block above it rather than starting a new one.
///
/// The non-obvious case is rustc's snippet gutter. It is right-aligned, so the
/// line-number rows of a snippet sit at column 0 whenever the gutter is as wide
/// as the number:
///
/// ```text
/// error[E0308]: mismatched types
///  --> src/main.rs:2:17
///   |
/// 2 |     let x: u8 = "s";
///   |            --   ^^^ expected `u8`, found `&str`
/// ```
///
/// Line 4 there begins in column 0 and is emphatically not a new diagnostic.
/// The `...` rustc prints for elided snippet rows lands in the gutter the same
/// way. Both are matched narrowly -- a bare column-0 number that is not followed
/// by the gutter bar is still an unrecognized line, and still loud.
fn is_continuation(line: &str) -> bool {
    if line.is_empty() || line.starts_with([' ', '\t']) {
        return true;
    }
    // The elision marker for skipped snippet rows.
    if line.starts_with("...") {
        return true;
    }
    // `<n> |`, the snippet gutter.
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && line[digits..].trim_start().starts_with('|')
}

/// The column-0 header line of a diagnostic block: a level, an optional error
/// code, a colon, and the primary message.
struct Head<'a> {
    level: &'a str,
    message: &'a str,
}

impl<'a> Head<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        // Longest first, so `note` cannot shadow a hypothetical longer level.
        for level in ["warning", "error", "note", "help"] {
            let Some(rest) = line.strip_prefix(level) else {
                continue;
            };
            // `error[E0308]: ...`
            let rest = match rest.strip_prefix('[') {
                Some(code) => &code[code.find(']')? + 1..],
                None => rest,
            };
            let message = rest.strip_prefix(':')?.trim_start();
            return Some(Head { level, message });
        }
        None
    }

    /// The counting lines that close out a compilation, from both cargo and
    /// rustc:
    ///
    /// ```text
    /// error: aborting due to 2 previous errors
    /// error: could not compile `nocompile-scratch` (bin "fixture") due to 2 previous errors
    /// warning: 3 warnings emitted
    /// warning: `nocompile-scratch` (bin "fixture") generated 1 warning
    /// ```
    ///
    /// Two reasons to drop all four. The cargo pair names the scratch crate and
    /// would leak `nocompile-scratch` into every golden. The rustc pair is a
    /// count, so adding one diagnostic to a fixture rewrites a line that
    /// describes nothing about the invariant under test.
    ///
    /// Only consulted for a block with no span, which is what keeps a fixture's
    /// own `compile_error!` from matching these by wording alone.
    fn is_summary(&self) -> bool {
        match self.level {
            "error" => {
                // `aborting due to 2 previous errors`
                starts_with_count(self.message.strip_prefix("aborting due to ").unwrap_or(""))
                    // `could not compile `x` (bin "y") due to 2 previous errors`
                    || (self.message.starts_with("could not compile `")
                        && self.message.contains(" due to "))
            }
            "warning" => {
                // `` `x` (bin "y") generated 1 warning ``
                (self.message.starts_with('`') && self.message.contains("` ")
                    && self.message.contains(" generated "))
                    // `3 warnings emitted`
                    || (starts_with_count(self.message) && self.message.ends_with(" emitted"))
            }
            _ => false,
        }
    }

    /// Cargo failing on its own terms. Distinguished from a fixture diagnostic
    /// by having no span: cargo's resolution and manifest errors describe the
    /// build, not the code. rustc messages that read the same way -- ``couldn't
    /// read `x.rs`` from an `include!`, say -- do carry a span and are kept.
    fn is_cargo_error(&self) -> bool {
        self.level == "error"
            && (self.message.starts_with("failed to ")
                || self.message.starts_with("no matching package")
                || self.message.starts_with("couldn't ")
                // `could not compile `x` (bin "y")` with no `due to` count is
                // cargo reporting that it could not *run* rustc, followed by a
                // `Caused by:` chain. The counted form is a summary and is
                // dropped above; this one is a genuine failure.
                || self.message.starts_with("could not compile `"))
    }
}

/// Whether `message` opens with a number, as every one of cargo's and rustc's
/// counting summaries does.
fn starts_with_count(message: &str) -> bool {
    message.starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(stderr: &str) -> String {
        classify(stderr).expect("should classify")
    }

    const DIAGNOSTIC: &str = "\
error[E0308]: mismatched types
 --> src/main.rs:2:17
  |
2 |     let x: u8 = \"s\";
  |            --   ^^^ expected `u8`, found `&str`
  |
";

    #[test]
    fn keeps_a_diagnostic_and_its_indented_body() {
        assert_eq!(kept(DIAGNOSTIC), DIAGNOSTIC.trim_end());
    }

    #[test]
    fn keeps_snippet_rows_whose_line_number_sits_in_column_zero() {
        let stderr = "\
error[E0080]: evaluation panicked
 --> src/main.rs:3:1
  |
3 | const _: () = assert!(false);
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^
...
9 |     more();
  |
";
        assert_eq!(kept(stderr), stderr.trim_end());
    }

    #[test]
    fn a_bare_column_zero_number_is_still_unrecognized() {
        let err = classify("error: x\n42 not a gutter\n").unwrap_err();
        assert!(matches!(err, ClassifyError::Unrecognized(_)), "{err:?}");
    }

    #[test]
    fn an_unrecognized_column_zero_line_is_a_hard_error() {
        let err = classify("Compiling nocompile-scratch v0.0.0\n").unwrap_err();
        let ClassifyError::Unrecognized(line) = err else {
            panic!("expected Unrecognized, got {err:?}");
        };
        assert_eq!(line, "Compiling nocompile-scratch v0.0.0");
    }

    #[test]
    fn drops_indented_status_lines_with_no_block_open() {
        let stderr = "    Blocking waiting for file lock on build directory\nerror: oh no\n";
        assert_eq!(kept(stderr), "error: oh no");
    }

    #[test]
    fn empty_stderr_classifies_to_nothing() {
        assert_eq!(kept(""), "");
    }

    // --- Summaries and footers, which have no span -------------------------

    #[test]
    fn drops_the_cargo_compile_summary() {
        let stderr = format!(
            "{DIAGNOSTIC}\nerror: aborting due to 1 previous error\n\n\
             error: could not compile `nocompile-scratch` (bin \"fixture\") due to 1 previous error\n"
        );
        assert_eq!(kept(&stderr), DIAGNOSTIC.trim_end());
    }

    #[test]
    fn drops_the_cargo_warning_summary() {
        let stderr = "\
warning: unused variable: `x`
 --> src/main.rs:2:9

warning: `nocompile-scratch` (bin \"fixture\") generated 1 warning
";
        assert_eq!(
            kept(stderr),
            "warning: unused variable: `x`\n --> src/main.rs:2:9"
        );
    }

    #[test]
    fn drops_the_rustc_warning_count() {
        let stderr = "warning: unused variable: `x`\nwarning: 1 warning emitted\n";
        assert_eq!(kept(stderr), "warning: unused variable: `x`");
    }

    #[test]
    fn drops_the_rustc_explain_footer() {
        let stderr = format!(
            "{DIAGNOSTIC}\nFor more information about this error, try `rustc --explain E0308`.\n"
        );
        assert_eq!(kept(&stderr), DIAGNOSTIC.trim_end());
    }

    #[test]
    fn drops_the_multi_error_explain_footer() {
        let stderr = "error: x\nSome errors have detailed explanations: E0308, E0433.\n";
        assert_eq!(kept(stderr), "error: x");
    }

    #[test]
    fn a_dropped_blocks_body_goes_with_it() {
        let stderr = "\
error: could not compile `nocompile-scratch` (bin \"fixture\") due to 1 previous error
  this line belongs to the summary
error: kept
";
        assert_eq!(kept(stderr), "error: kept");
    }

    // --- A fixture's own diagnostic must never be mistaken for a summary ----
    //
    // A derive is free to phrase its `compile_error!` however it likes. These
    // are the wordings that a message-text match would swallow, deleting the
    // invariant under test from its own golden.

    #[test]
    fn keeps_a_fixture_error_worded_like_an_abort_summary() {
        let stderr = "\
error: aborting due to a shape this derive cannot support
 --> src/main.rs:1:1
  |
1 | #[derive(Codec)]
  | ^^^^^^^^^^^^^^^^
";
        assert_eq!(kept(stderr), stderr.trim_end());
    }

    #[test]
    fn keeps_a_fixture_error_worded_like_a_cargo_failure() {
        let stderr = "\
error: failed to parse the codec attribute: expected a literal
 --> src/main.rs:1:10
  |
1 | #[derive(Codec)]
  |          ^^^^^
";
        assert_eq!(kept(stderr), stderr.trim_end());
    }

    #[test]
    fn keeps_a_rustc_error_worded_like_a_cargo_failure() {
        // `include!` of a missing file. Genuine rustc, genuine span.
        let stderr = "\
error: couldn't read `src/nope.rs`: No such file or directory
 --> src/main.rs:1:1
  |
1 | include!(\"nope.rs\");
  | ^^^^^^^^^^^^^^^^^^^
";
        assert_eq!(kept(stderr), stderr.trim_end());
    }

    #[test]
    fn keeps_a_summary_worded_block_that_carries_a_span() {
        let stderr = "\
error: could not compile `something` due to a reason
 --> src/main.rs:1:1
";
        assert_eq!(kept(stderr), stderr.trim_end());
    }

    #[test]
    fn an_abort_summary_needs_a_count_to_be_dropped() {
        // The real thing counts; a fixture's prose does not.
        assert_eq!(kept("error: aborting due to 2 previous errors\n"), "");
        assert_eq!(
            kept("error: aborting due to the union above\n"),
            "error: aborting due to the union above"
        );
    }

    // --- Cargo's own failures ----------------------------------------------

    #[test]
    fn a_failure_to_run_rustc_is_a_cargo_failure_with_its_chain() {
        let stderr = "\
error: could not compile `nocompile-scratch` (bin \"fixture\")

Caused by:
  process didn't exit successfully: `rustc ...` (exit status: 1)
";
        let err = classify(stderr).unwrap_err();
        let ClassifyError::Cargo(message) = err else {
            panic!("expected Cargo, got {err:?}");
        };
        assert!(message.contains("Caused by:"), "{message}");
        assert!(message.contains("exit status: 1"), "{message}");
    }

    #[test]
    fn cargo_resolution_failures_are_not_diagnostics() {
        let err = classify("error: failed to select a version for `x`\n").unwrap_err();
        let ClassifyError::Cargo(message) = err else {
            panic!("expected Cargo, got {err:?}");
        };
        assert_eq!(message, "error: failed to select a version for `x`");
    }

    #[test]
    fn an_error_against_the_generated_manifest_is_a_cargo_failure() {
        let stderr = "\
error: key with no value, expected `=`
  --> Cargo.toml:16:6
   |
16 | this is not valid toml [[[
";
        let err = classify(stderr).unwrap_err();
        let ClassifyError::Cargo(message) = err else {
            panic!("expected Cargo, got {err:?}");
        };
        assert!(message.contains("expected `=`"), "{message}");
    }

    #[test]
    fn a_manifest_warning_is_left_to_the_golden() {
        let stderr = "warning: unused manifest key: package.foo\n --> Cargo.toml:5:1\n";
        assert_eq!(kept(stderr), stderr.trim_end());
    }
}
