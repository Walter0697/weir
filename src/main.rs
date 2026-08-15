//! Syncs a fork with its upstream and opens a pull request for review.
//!
//! The binary performs one pass and exits. Deciding *when* it runs belongs
//! outside it — a scheduler, a cron, a CI workflow, or the `serve` command once
//! that exists.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use weir::config::Config;

#[derive(Parser)]
#[command(name = "weir", version, about)]
struct Cli {
    /// Path to the fork list.
    #[arg(long, short, default_value = "forks.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse the config and report what would be synced, touching nothing.
    Validate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate => validate(&cli.config),
    }
}

fn validate(path: &std::path::Path) -> Result<()> {
    let config = Config::load(path)?;
    println!(
        "config v{} — {} — {} fork(s), boundary file {:?}, sync branch {:?}",
        config.version,
        config.forge_url(),
        config.forks.len(),
        config.defaults.boundary_file,
        config.defaults.sync_branch,
    );
    for fork in &config.forks {
        println!(
            "  {}/{}: {} ({} -> {})",
            config.forge.owner,
            fork.repo,
            fork.upstream,
            fork.upstream_branch(),
            fork.branch,
        );
        for path in &fork.keep_removed {
            println!("    keeps removed: {path}");
        }
    }
    Ok(())
}
