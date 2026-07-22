use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code =
        compresso_eval::cli::run(std::env::args_os(), &mut stdout.lock(), &mut stderr.lock());
    ExitCode::from(code.value())
}
