// The other half of a UI suite: proof that the allowed form still compiles.
// Without one, nothing in the suite says the API is usable at all.
fn takes_two(a: u8, b: u8) -> u8 {
    a + b
}

fn main() {
    let total = takes_two(1, 2);
    let _x: u8 = total;
    let owned = String::from("lives long enough");
    let borrowed = &owned;
    println!("{borrowed} {total}");
}
