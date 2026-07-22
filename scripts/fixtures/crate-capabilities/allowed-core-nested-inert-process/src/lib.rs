use std::{process::{ExitCode, id}};

pub fn inert_process_metadata() -> (u32, ExitCode) {
    (id(), ExitCode::SUCCESS)
}
