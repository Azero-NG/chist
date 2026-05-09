mod cli;
mod config;
mod db;
mod hooks;
mod index;
mod output;
mod parse;
mod search;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Search(opts) => search::run(opts),
        cli::Command::Rebuild(opts) => index::rebuild(opts),
        cli::Command::Sync(opts) => index::run_sync(opts),
        cli::Command::InstallHook => hooks::install(),
        cli::Command::UninstallHook => hooks::uninstall(),
        cli::Command::Stats => db::print_stats(),
    }
}
