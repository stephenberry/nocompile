//! A library's own error code lives in the message, because rustc's `E0xxx`
//! registry is closed. `Brief` keeps primary messages, so the token is compared.

macro_rules! derive_serialize {
    ($t:ty) => {
        compile_error!("MYLIB-E001: expected a struct with named fields");
    };
}

derive_serialize!(u8);

fn main() {}
