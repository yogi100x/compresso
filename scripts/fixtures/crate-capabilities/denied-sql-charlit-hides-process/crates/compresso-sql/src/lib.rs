#![deny(unsafe_code)]
pub fn e01() { let _q = '"'; let _ = tokio::process::Command::new("sh"); }
