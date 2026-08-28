//! A plain-text unified-ish diff.
//!
//! No colour: `termcolor` is a dependency whose whole job is to make CI logs
//! prettier, and CI logs are usually read without colour anyway.

use std::fmt::Write as _;

/// Lines of context kept around each change.
const CONTEXT: usize = 3;

/// Above this many cells the quadratic LCS is skipped in favour of a blunt
/// delete-then-insert rendering. Goldens are small; this only guards against a
/// pathological one.
const MAX_CELLS: usize = 1 << 20;

/// The 1-based number of the first line at which `expected` and `actual` differ.
pub(crate) fn first_difference(expected: &str, actual: &str) -> Option<usize> {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    for (i, (e, a)) in expected.iter().zip(&actual).enumerate() {
        if e != a {
            return Some(i + 1);
        }
    }
    if expected.len() == actual.len() {
        None
    } else {
        Some(expected.len().min(actual.len()) + 1)
    }
}

/// Render the difference between two texts, with `-` for expected-only lines and
/// `+` for actual-only ones.
pub(crate) fn unified(
    expected: &str,
    actual: &str,
    expected_label: &str,
    actual_label: &str,
) -> String {
    let a: Vec<&str> = expected.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    let edits = diff(&a, &b);

    let mut out = String::new();
    let _ = writeln!(out, "--- expected ({expected_label})");
    let _ = writeln!(out, "+++ actual ({actual_label})");

    // Collapse runs of unchanged lines longer than twice the context.
    let mut i = 0;
    while i < edits.len() {
        let Edit::Same(_) = edits[i] else {
            render(&mut out, &edits[i]);
            i += 1;
            continue;
        };
        let run_end = edits[i..]
            .iter()
            .position(|e| !matches!(e, Edit::Same(_)))
            .map_or(edits.len(), |n| i + n);
        let run = run_end - i;

        if run <= CONTEXT * 2 + 1 || (i == 0 && run <= CONTEXT) {
            for edit in &edits[i..run_end] {
                render(&mut out, edit);
            }
        } else {
            let head = if i == 0 { 0 } else { CONTEXT };
            let tail = if run_end == edits.len() { 0 } else { CONTEXT };
            for edit in &edits[i..i + head] {
                render(&mut out, edit);
            }
            let _ = writeln!(out, "@@ {} unchanged line(s) @@", run - head - tail);
            for edit in &edits[run_end - tail..run_end] {
                render(&mut out, edit);
            }
        }
        i = run_end;
    }

    // Reachable via an empty-but-present golden, or a `Codes` filter that
    // reduces an `Exact` golden to nothing. The mirrored case -- empty `actual`
    // -- cannot occur, because `Failure::NoDiagnostics` returns before the
    // golden is read.
    if a.is_empty() {
        let _ = writeln!(out, "(the golden is empty)");
    }
    out
}

fn render(out: &mut String, edit: &Edit<'_>) {
    let (prefix, line) = match edit {
        Edit::Same(line) => (' ', line),
        Edit::Delete(line) => ('-', line),
        Edit::Insert(line) => ('+', line),
    };
    let _ = writeln!(out, "{prefix}{line}");
}

#[derive(Debug, PartialEq, Eq)]
enum Edit<'a> {
    Same(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

fn diff<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Edit<'a>> {
    // Trimming the common prefix and suffix keeps the quadratic core off the
    // usual case, which is one changed line in an otherwise identical golden.
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let remaining = a.len().min(b.len()) - prefix;
    let suffix = (0..remaining)
        .take_while(|i| a[a.len() - 1 - i] == b[b.len() - 1 - i])
        .count();

    let mut out: Vec<Edit<'a>> = a[..prefix].iter().copied().map(Edit::Same).collect();

    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];
    if a_mid.len().saturating_mul(b_mid.len()) > MAX_CELLS {
        out.extend(a_mid.iter().copied().map(Edit::Delete));
        out.extend(b_mid.iter().copied().map(Edit::Insert));
    } else {
        out.extend(lcs(a_mid, b_mid));
    }

    out.extend(a[a.len() - suffix..].iter().copied().map(Edit::Same));
    out
}

/// Longest-common-subsequence diff. Quadratic, which is fine at golden sizes and
/// is guarded by [`MAX_CELLS`] above.
fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Edit<'a>> {
    let (n, m) = (a.len(), b.len());
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(Edit::Same(a[i]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            out.push(Edit::Delete(a[i]));
            i += 1;
        } else {
            out.push(Edit::Insert(b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().copied().map(Edit::Delete));
    out.extend(b[j..].iter().copied().map(Edit::Insert));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_have_no_first_difference() {
        assert_eq!(first_difference("a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn first_difference_is_one_based() {
        assert_eq!(first_difference("a\nb\nc\n", "a\nX\nc\n"), Some(2));
    }

    #[test]
    fn a_truncated_text_differs_at_the_first_missing_line() {
        assert_eq!(first_difference("a\nb\n", "a\n"), Some(2));
        assert_eq!(first_difference("", "a\n"), Some(1));
    }

    #[test]
    fn diff_marks_a_replaced_line() {
        let out = unified("a\nb\nc\n", "a\nX\nc\n", "golden", "actual");
        assert_eq!(
            out,
            "--- expected (golden)\n+++ actual (actual)\n a\n-b\n+X\n c\n"
        );
    }

    #[test]
    fn diff_marks_an_inserted_line() {
        let out = unified("a\nc\n", "a\nb\nc\n", "g", "a");
        assert!(out.contains("+b\n"), "{out}");
        assert!(!out.contains("-a"), "{out}");
    }

    #[test]
    fn long_unchanged_runs_are_collapsed() {
        let a: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut b: Vec<String> = (0..40).map(|i| format!("line {i}\n")).collect();
        b[20] = "changed\n".to_string();
        let out = unified(&a, &b.concat(), "g", "a");
        assert!(out.contains("@@"), "{out}");
        assert!(out.contains("-line 20"), "{out}");
        assert!(out.contains("+changed"), "{out}");
        assert!(!out.contains("line 5"), "{out}");
    }

    #[test]
    fn an_empty_golden_is_called_out() {
        let out = unified("", "error: x\n", "g", "a");
        assert!(out.contains("(the golden is empty)"), "{out}");
        assert!(out.contains("+error: x"), "{out}");
    }

    #[test]
    fn two_separated_changes_interleave_rather_than_bulk_replace() {
        // The prefix/suffix trim cannot reduce this: the changed lines sit at
        // both ends of the middle, so only the LCS keeps `c`, `d` and `e`
        // recognized as unchanged. This is the shape a rustc release produces
        // when it reflows several messages in one golden, and without the LCS
        // the whole middle renders as N deletes followed by N inserts.
        let edits = diff(
            &["a", "B", "c", "d", "e", "F", "g"],
            &["a", "x", "c", "d", "e", "y", "g"],
        );
        assert_eq!(
            edits,
            vec![
                Edit::Same("a"),
                Edit::Delete("B"),
                Edit::Insert("x"),
                Edit::Same("c"),
                Edit::Same("d"),
                Edit::Same("e"),
                Edit::Delete("F"),
                Edit::Insert("y"),
                Edit::Same("g"),
            ]
        );
    }

    #[test]
    fn lcs_prefers_a_common_subsequence_over_wholesale_replacement() {
        let edits = diff(&["a", "b", "c", "d"], &["a", "x", "c", "d"]);
        assert_eq!(
            edits,
            vec![
                Edit::Same("a"),
                Edit::Delete("b"),
                Edit::Insert("x"),
                Edit::Same("c"),
                Edit::Same("d"),
            ]
        );
    }
}
