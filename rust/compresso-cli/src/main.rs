use std::io;
use std::process::ExitCode;

use clap::Parser;
use compresso_cli::{Cli, UnavailableAccountClient, execute};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code = execute(
        cli,
        &UnavailableAccountClient,
        &mut stdout.lock(),
        &mut stderr.lock(),
    );
    ExitCode::from(code.value())
}
