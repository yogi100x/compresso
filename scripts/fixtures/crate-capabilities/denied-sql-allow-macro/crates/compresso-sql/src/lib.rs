#![deny(unsafe_code)]
macro_rules! quiet { ($l:ident, $($i:item)*) => { $(#[allow(clippy::$l)] $i)* } }
