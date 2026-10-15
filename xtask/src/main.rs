//! `cargo xtask <task>` — project automation (AGENTS.md §2 Phase 3).

mod ci;
mod e2e;
mod golden;
mod layers;
mod parity;
mod protect;

use anyhow::{Result, bail};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <ci|layers|golden [--review|--approve <name>…]|parity [--raise]|bench [all|app-smoke|latency|startup]|prettify-gallery|protect [--write]>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("layers") => layers::check(),
        Some("golden") => golden::main(args.get(1..).unwrap_or(&[])),
        Some("parity") => parity::main(args.get(1..).unwrap_or(&[])),
        Some("ci") => ci::ci(),
        Some("bench") => ci::bench(args.get(1..).unwrap_or(&[])),
        Some("prettify-gallery") => ci::prettify_gallery(),
        Some("protect") if args.get(1).is_some_and(|a| a == "--write") => protect::write(),
        Some("protect") => {
            for l in protect::report()? {
                eprintln!("{l}");
            }
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
