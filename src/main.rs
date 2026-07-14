use std::process::ExitCode;

fn main() -> ExitCode {
    xmlcarve::cli::run(std::env::args().skip(1).collect())
}
