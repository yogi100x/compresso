#![deny(unsafe_code)]
extern crate tokio as tk07; use tk07::{process as p07}; pub fn e07() { let _ = p07::Command::new("sh"); }
