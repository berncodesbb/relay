//! Relay native command-line executable entry point.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // `--json` is per-command; the dispatcher builds the writer after
    // parsing so help/version output is never suppressed.
    let json = argv
        .iter()
        .any(|argument| argument == "--json" || argument.starts_with("--json="));
    let mut writer = relay_cli::output::OutputWriter::std(json);
    let code = if argv
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        let mut plain = relay_cli::output::OutputWriter::std(false);
        relay_cli::run_main(&argv, &mut plain)
    } else {
        relay_cli::run_main(&argv, &mut writer)
    };
    std::process::exit(code);
}
