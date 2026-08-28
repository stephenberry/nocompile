# nobuild

**Assert that code does _not_ compile.** A compile-fail test harness that depends on nothing but `std` — dev-dependencies included.

A compile-fail test asserts that a program does not build, and that it fails for the intended reason. That is a class of invariant no runtime test can express, because the whole point is that the offending code never exists as a binary:

- a derive that must refuse a shape it cannot support
- a macro whose generated identifiers must stay unnameable from the caller's body
- a sealed trait that must reject an `impl` from outside its crate
- a const assertion that must fire at compile time rather than on someone else's machine
- an API designed so a reference cannot escape a closure

Every one of these is enforced by the type system, by const evaluation, or by macro hygiene. Without a test that tries to break the guard and observes the error, a refactor can quietly remove it while every runtime test still passes.

## Usage

```rust
// tests/ui.rs
#[test]
fn ui() {
    let mut t = nobuild::cases!();
    t.dependency_path("my-crate", ".");   // fixtures need the crate under test
    t.compile_fail_dir("tests/ui");       // every .rs beside its .stderr
    t.assert();
}
```

Each fixture is compiled on its own. A `compile_fail` fixture must fail, and its diagnostics must match the `.stderr` golden beside it.

```
tests/ui/rejects_union.rs
tests/ui/rejects_union.stderr
```

Write the goldens with `NOBUILD=overwrite cargo test`, then **read what they captured**. A missing golden is a failure rather than an implicit bless precisely so that step does not get skipped — otherwise a new fixture passes on the run that creates it and nobody looks at what it recorded.

Everything else is opt-in:

```rust
t.mode(nobuild::Mode::Codes);                 // less brittle comparison, below
t.compile_fail("tests/ui/just_this_one.rs");
t.pass_dir("tests/ui-pass");                  // fixtures that must still compile
t.edition("2024");
t.raw_manifest_lines("[features]\nfoo = []"); // escape hatch
let outcome = t.run();                        // non-panicking, returns a report
```

One struct, ten methods, two enums. That is the whole library.

## Living with toolchain churn

`.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent to golden-matching rendered text, and it is the worst property of this style of test. `nobuild` cannot fix it, but it offers a cheaper mode — the one axis on which it is _better_ than the alternative rather than merely lighter:

```rust
t.mode(nobuild::Mode::Codes);
```

`Codes` compares only error codes, primary messages and span headers:

```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
--> tests/ui/wrong_arity.rs:5:5
--> tests/ui/wrong_arity.rs:2:4
```

instead of the full rendering:

```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
 --> tests/ui/wrong_arity.rs:5:5
  |
5 |     takes_two(1);
  |     ^^^^^^^^^--- argument #2 of type `u8` is missing
  |
note: function defined here
 --> tests/ui/wrong_arity.rs:2:4
  |
2 | fn takes_two(_a: u8, _b: u8) {}
  |    ^^^^^^^^^         ------
help: provide the argument
  |
5 |     takes_two(1, /* u8 */);
  |                ++++++++++
```

What it drops is entirely rustc-rendering detail — source snippets, underline art, and the `= note:` lines that a rustc release reflows. What it keeps is every error code, every primary message and every span, so it still catches every regression that matters: a fixture that stops failing, or one that starts failing for a _different_ reason. On one real 19-fixture suite it takes 436 golden lines down to 78.

The filter is applied to both sides of the comparison, so an existing `Exact` golden passes in `Codes` mode unchanged. Switching is a one-line change; blessing afterwards shrinks the goldens to match.

## Zero dependencies, dev-dependencies included

A crate whose selling point is "no dependencies" cannot have dev-dependencies either. A `[dev-dependencies]` entry shows up in `cargo tree` for anyone vendoring or auditing the source, and a harness that reaches for a helper crate to test itself has undermined its own pitch. `nobuild`'s own compile-fail suite is run by `nobuild`, and everything else is `assert_eq!` on strings.

## Should you use this?

**Probably not.** [`trybuild`] is the standard answer, it is battle-tested across thousands of crates, and it is more capable. This is not a criticism of it — it is a different point on the dependency/complexity curve.

Use `nobuild` if you keep a deliberately small dependency surface and currently pay a large one for a handful of compile-fail fixtures: `no_std`-adjacent crates, cryptography and safety-critical libraries, anything audited, anything embedded, anything whose pitch is its dependency tree. In one real workspace, `trybuild` was the only root of fifteen lock entries:

```
glob  serde  serde_core  serde_derive  serde_json  itoa  memchr  zmij
target-triple  termcolor  toml  serde_spanned  toml_datetime  toml_parser  winnow
```

That set is workspace-dependent — if you already depend on `serde` or `toml`, removing `trybuild` removes correspondingly fewer. Check your own with `cargo tree -i -p <crate>`.

And be honest about the size of the win: `trybuild` is a dev-dependency. It never ships and never enters a release binary. It costs lock entries, some test-build time, and an explanation when an audit asks why a compile-fail harness needs a serialization framework. If you do not track your dependency count, use `trybuild`.

[`trybuild`]: https://docs.rs/trybuild

## Scope

**In:** `compile_fail` fixtures with `.stderr` goldens, `pass` fixtures, a bless mode, and the two comparison modes.

**Out, deliberately:**

| | |
|---|---|
| Running the compiled program and checking its output | That is [`trycmd`](https://docs.rs/trycmd)/[`assert_cmd`](https://docs.rs/assert_cmd) territory and a different problem. |
| Glob patterns | A directory or an explicit file covers every real use and costs no matcher. This is why `trybuild` needs `glob`. |
| Inferring dependencies from the host manifest | The single biggest simplification, and also better behaviour — see below. |
| Cross-compilation, custom targets, `-Z` flags, nightly-only features | A compile-fail suite runs on the host toolchain. If you need more, you need `trybuild`. |
| Windows | Not in v1. Path normalization and the `\r\n` question need someone with a Windows machine to get right, and claiming support without testing it is worse than not claiming it. The normalization is kept in one function so it is a contained addition later. |

The moment a suite grows past what the host toolchain can express, `trybuild` is the answer and this crate would rather say so than grow toward it.

## Declared dependencies, not inferred ones

`nobuild` writes the scratch project's manifest instead of reading yours. That removes both a TOML parser and a `cargo metadata` invocation, and it is also tighter: inference hands every fixture every dev-dependency of the host crate, so a fixture can quietly lean on something the invariant under test never mentions. Explicit is both cheaper and stricter. In the common case it is one line.

```rust
t.dependency_path("my-crate", ".");
t.raw_manifest_lines("[features]\nfoo = []");   // for anything exotic
```

## Requirements on fixtures

- A fixture is built as a bin and compiled **verbatim**, so it must define `fn main`, as `trybuild` fixtures do. The harness does not add one: detecting a real `fn main` needs a parser, and a wrong guess writes harness-injected source into the golden under the fixture's own name. A fixture without one gets a plain `E0601`, which says what to do about it.
- Fixtures build with `--offline`, so a dependency must be a path dependency or already in the local cargo cache. A compile-fail suite that can reach the network is a suite that fails in CI for unrelated reasons.
- Warnings from the crate under test land in the fixture's stderr and so in its golden, exactly as they do with `trybuild`. Keep the crate under test warning-clean, or use `Mode::Codes`.
- `RUSTFLAGS` is cleared for the fixture build, including `[build] rustflags` from any `.cargo/config.toml`. An inherited `-D warnings` would turn every fixture's warning into an error and silently change what the goldens contain.
- A diagnostic message beginning with `aborting due to` is suppressed by **cargo itself** before any harness can see it, because that is where cargo filters rustc's own abort line. If a `compile_error!` in your crate is worded that way it will never reach a golden; word it differently.

## Concurrency

Every fixture in a run is written to the same scratch `src/main.rs`, so a run holds an exclusive lock on its scratch project and concurrent runs serialize. Two `#[test]` functions each calling `nobuild::cases!()` is safe, as is `cargo nextest` or two `cargo test` invocations at once. Without the lock they would compile each other's fixtures and report a broken fixture as passing.

## How it works

`trybuild` runs `cargo build --message-format=json` and reads each diagnostic's pre-rendered `rendered` field, which is why it needs a JSON stack. But `rendered` is byte-for-byte what plain stderr prints — cargo renders it and the JSON just carries the same string. So `nobuild` reads plain stderr with `--quiet`, which is already almost exactly the golden.

What remains is cargo's and rustc's own summary lines, which name the scratch crate and count the errors. Those are **classified** out rather than filtered out: every line at column 0 is matched against a known shape, and one that matches nothing is a hard error naming the line. The failure mode of a silent filter is garbage creeping into goldens; the failure mode of this is a loud, actionable message the first time cargo changes its output.

Normalization is five substitutions and is meant to stay that way — every substitution is something a golden can no longer distinguish:

| | |
|---|---|
| `src/main.rs` | the fixture's own relative path |
| the scratch project | `$SCRATCH` |
| the host manifest directory | `$DIR` |
| an unpacked registry source directory | `$CARGO_REGISTRY` |
| `CARGO_HOME` | `$CARGO_HOME` |

Plus `\r\n` to `\n`, trailing whitespace stripped per line, and exactly one trailing newline.

## Migrating from trybuild

Goldens are usually close but not portable verbatim, since the normalization differs. Re-bless with `NOBUILD=overwrite` and **read the diff line by line** — a migration that blesses without reading silently accepts whatever the new harness produces, including nothing at all. Then confirm the dependencies actually left with `cargo tree -i -p <each>`.

## MSRV

Rust 1.96, edition 2024.

## License

MIT OR Apache-2.0.
