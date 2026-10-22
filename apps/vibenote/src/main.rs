//! The vibenote desktop app.

use std::process::ExitCode;

const USAGE: &str = "usage: vibenote [--workspace <path.sqlite>] [--note <id>] | --install-desktop-entry";

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
            // Adds vibenote to the app launcher for this user (R-PLAT-004).
            Some("--install-desktop-entry") => {
                let dirs = vibenote_engine::desktop::Dirs::from_env();
                let launcher = vibenote_engine::desktop::launcher();
                return match dirs.zip(launcher) {
                    Some((d, l)) => match vibenote_engine::desktop::install(&d, &l) {
                        Ok(o) => {
                            eprintln!("{}", o.message());
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("vibenote: {e}");
                            ExitCode::FAILURE
                        }
                    },
                    None => {
                        eprintln!("vibenote: neither XDG_DATA_HOME nor HOME is set");
                        ExitCode::FAILURE
                    }
                };
            }
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
    // As an AppImage, vibenote puts itself into the app launcher (R-PLAT-004).
    match vibenote_engine::desktop::auto_install() {
        Some(Ok(o)) if !matches!(o, vibenote_engine::desktop::Outcome::Unchanged(_)) => {
            eprintln!("vibenote: {}", o.message())
        }
        Some(Err(e)) => eprintln!("vibenote: desktop entry: {e}"),
        _ => {}
    }
    match vibenote_ui::run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vibenote: {e}");
            ExitCode::FAILURE
        }
    }
}
