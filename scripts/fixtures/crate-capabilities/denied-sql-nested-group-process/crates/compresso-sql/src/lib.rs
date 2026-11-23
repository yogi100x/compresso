#![deny(unsafe_code)]
use tokio::{io::{self as _tio}, process::Command as Cmd02}; pub fn e02() { let _ = Cmd02::new("sh"); }
