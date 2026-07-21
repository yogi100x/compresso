use std as platform;

pub fn forbidden_spawn() {
    let _command = platform::process::Command::new("echo");
}
