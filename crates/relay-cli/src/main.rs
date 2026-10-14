//! Relay native command-line executable entry point.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // `relay_cli::run` builds the writer before dispatch (a plain writer
    // for `--help`/`-h` so help output is never suppressed) and carries
    // the detected terminal capability, which decides whether the chat
    // interface may open.
    let code = relay_cli::run(&argv, relay_cli::detect_terminal_capability());
    std::process::exit(code);
}
