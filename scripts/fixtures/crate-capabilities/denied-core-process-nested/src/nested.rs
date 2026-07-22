use std::{process::{Command as Run}};

pub fn forbidden_spawn() {
    let _child = Run::new("echo").spawn();
}
