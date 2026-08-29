//! A macro reporting misuse over more than one line.
//!
//! `Brief` keeps the whole message, not just its first line: those lines were
//! split where this fixture split them, so a change to the second one is a
//! change to the assertion. Without that, this golden and one ending `help:
//! derive on a struct, not a union` would compare equal.

compile_error!("MYLIB-E002: expected a struct with named fields\nhelp: derive on a struct, not an enum");

fn main() {}
