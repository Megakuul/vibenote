//! The vibenote desktop app.

use std::process::ExitCode;

const USAGE: &str = "usage: vibenote [--workspace <path.sqlite>] [--note <id>]";

fn main() -> ExitCode {
    let mut opts = vibenote_ui::Options::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--workspace") => match args.next() {
                Some(p) => opts.workspace = Some(p.into()),
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            Some("--note") => match args.next().and_then(|v| v.to_str().and_then(|s| s.parse().ok())) {
                Some(id) => opts.note = Some(id),
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            Some("-h" | "--help") => {
                eprintln!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match vibenote_ui::run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vibenote: {e}");
            ExitCode::FAILURE
        }
    }
}
