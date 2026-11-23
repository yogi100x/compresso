#![deny(unsafe_code)]
use tokio::*; pub fn f() { let _ = process::Command::new("sh"); }
