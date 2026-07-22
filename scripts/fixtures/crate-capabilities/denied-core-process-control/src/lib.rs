use std::process::{Command, abort, exit};

pub fn forbidden_process_control() {
    let _child = Command::new("echo").spawn();
    if std::hint::black_box(false) {
        exit(1);
    }
    abort();
}
