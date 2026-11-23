// D117: no unsafe code (no FFI process spawning); the capability check
// requires this line.
#![forbid(unsafe_code)]

use std::io;
use std::process::ExitCode;

use clap::Parser;
use compresso_cli::{Cli, UnavailableAccountClient, execute};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let code = execute(cli, &UnavailableAccountClient, &mut stdout, &mut stderr);
    ExitCode::from(code.value())
}
