use std as platform;
use platform::{process::Command};

pub fn forbidden_spawn() {
    let _child = Command::new("echo").spawn();
}
