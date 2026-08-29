//! A guard that only exists after monomorphization.
//!
//! `cargo check` stops before codegen, so it never instantiates `split::<3>`
//! and reports nothing at all. This fixture is what pins the harness to
//! `cargo build`: if it ever moved to `check`, this golden would go empty.

pub fn split<const N: usize>() {
    const { assert!(N.is_power_of_two(), "N must be a power of two") };
}

fn main() {
    split::<3>();
}
