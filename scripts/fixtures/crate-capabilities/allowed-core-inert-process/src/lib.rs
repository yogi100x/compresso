use std::process;
use std::process::ExitCode;

pub fn inert_process_metadata() -> (u32, ExitCode) {
    (process::id(), std::process::ExitCode::SUCCESS)
}
