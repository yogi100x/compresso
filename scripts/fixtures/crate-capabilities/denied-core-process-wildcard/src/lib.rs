use std::process::*;

pub fn wildcard_can_spawn() {
    let _child = Command::new("echo").spawn();
}
