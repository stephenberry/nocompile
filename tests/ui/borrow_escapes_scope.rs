// A borrow invariant: the reference must not outlive what it points at. An API
// designed so a reference cannot escape has that property only if the escaping
// version does not compile.
fn main() {
    let outer;
    {
        let inner = String::from("gone at the end of this block");
        outer = &inner;
    }
    println!("{outer}");
}
