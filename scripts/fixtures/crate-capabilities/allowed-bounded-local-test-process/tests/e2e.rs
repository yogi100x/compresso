use std::process::{Command, Stdio};

#[test]
fn test_process_harness_can_construct_a_command() {
    let mut command = Command::new("fixture-command");
    command.stdin(Stdio::null());
}
