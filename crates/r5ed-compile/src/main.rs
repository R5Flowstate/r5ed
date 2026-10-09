//! The `r5ed-compile` entry point: parse the command line, run the verb.

use clap::Parser;

fn main() -> anyhow::Result<()> {
    r5ed_compile::run(r5ed_compile::Cli::parse())
}
