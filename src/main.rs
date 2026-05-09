mod cli;
mod db;
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
        cli::Command::Stats => db::print_stats(),
    }
}
