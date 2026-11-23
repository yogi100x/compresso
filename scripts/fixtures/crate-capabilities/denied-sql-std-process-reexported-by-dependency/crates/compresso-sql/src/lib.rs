#![deny(unsafe_code)]
pub fn spawn() { let _ = mongodb::process::Command::new("sh"); }
