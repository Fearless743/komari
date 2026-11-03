use std::process::ExitCode;

fn main() -> ExitCode {
    match komari_cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
