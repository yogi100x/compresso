//! Prints the public surface inventory as JSON for the documentation drift
//! linter: `cargo run -p compresso-cli --example surface_inventory`.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let inventory = compresso_cli::surface::surface_inventory();
    let mut output = io::stdout().lock();
    match serde_json::to_writer_pretty(&mut output, &inventory)
        .map_err(io::Error::other)
        .and_then(|()| writeln!(output))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("failed to write surface inventory: {error}");
            ExitCode::FAILURE
        }
    }
}
