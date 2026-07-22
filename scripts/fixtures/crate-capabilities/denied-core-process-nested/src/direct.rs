use std::{process::Command};

pub fn forbidden_spawn() {
    let _child = Command::new("echo").spawn();
}
