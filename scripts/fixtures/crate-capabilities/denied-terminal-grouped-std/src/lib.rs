use std::{process as proc};

pub fn forbidden_spawn() {
    let _command = proc::Command::new("echo");
}
