//! Syncs a fork with its upstream and opens a pull request for the result.
//!
//! Two front ends over one library. `run` performs a single pass from a TOML
//! file and exits — deciding *when* that happens belongs outside it. `serve`
//! keeps its configuration in a database, draws a UI for it, and owns a
//! schedule. They never share a source of truth, so there is always exactly one
//! answer to where a setting came from.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use weir::config::{Config, Fork, Notify};
use weir::notify::{self, Notifier};
use weir::runner::{self, ForgeSpec, ForkSpec, Options};
use weir::store::Store;

#[derive(Parser)]
#[command(name = "weir", version, about)]
struct Cli {
    /// Path to the fork list. Used by `validate` and `run`; `serve` reads its
    /// configuration from the database instead.
    #[arg(long, short, default_value = "forks.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse the config and report what would be synced, touching nothing.
    Validate,
    /// Sync each fork once and report what happened.
    Run {
        /// Only this fork, rather than every one in the config.
        #[arg(long)]
        repo: Option<String>,
        /// Do everything except the parts that cannot be undone: no push, no
        /// pull request. Safe to point at a live forge.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the web UI and the scheduler.
    Serve {
        /// Where the database lives. It holds the forge token, so it is created
        /// readable only by its owner.
        #[arg(long, default_value = "weir.db")]
        db: PathBuf,
        /// Loopback by default: anything that reaches this can change which
        /// repositories get force-pushed.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate => validate(&cli.config),
        Command::Run { repo, dry_run } => run(&cli.config, repo.as_deref(), dry_run),
        Command::Serve { db, bind } => serve(&db, bind),
    }
}

fn serve(db: &Path, bind: SocketAddr) -> Result<()> {
    let store = Store::open(db)?;
    // Only this path needs an async runtime, so the rest of the binary stays
    // plainly blocking.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?
        .block_on(weir::serve::serve(store, bind))
}

fn validate(path: &Path) -> Result<()> {
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
    // Notification channels are configuration too, and a silent one is the
    // hardest kind of misconfiguration to notice.
    if config.notify.is_empty() {
        println!("  notifications: none configured");
    }
    for channel in &config.notify {
        match channel {
            Notify::Telegram {
                token_env,
                chat_env,
            } => println!(
                "  notifications: telegram (reads {token_env} and {chat_env}) — {}",
                match (env_value(token_env), env_value(chat_env)) {
                    (Some(_), Some(_)) => "both set",
                    (None, Some(_)) => "TOKEN MISSING, will stay silent",
                    (Some(_), None) => "CHAT ID MISSING, will stay silent",
                    (None, None) => "neither set, will stay silent",
                }
            ),
        }
    }
    Ok(())
}

fn run(config_path: &Path, only: Option<&str>, dry_run: bool) -> Result<()> {
    let config = Config::load(config_path)?;

    let selected: Vec<&Fork> = match only {
        Some(name) => {
            let picked: Vec<&Fork> = config.forks.iter().filter(|f| f.repo == name).collect();
            anyhow::ensure!(
                !picked.is_empty(),
                "no fork named {name:?} in {}; it lists {}",
                config_path.display(),
                config
                    .forks
                    .iter()
                    .map(|f| f.repo.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            picked
        }
        None => config.forks.iter().collect(),
    };

    // Trimmed, because a token routinely arrives with a trailing newline — from
    // a pasted heredoc, a docker `--env-file`, or a mounted secret — and the
    // resulting authentication failure says nothing about why.
    let token = env_value(&config.forge.token_env);
    if token.is_none() {
        eprintln!(
            "note: {} is unset, so the forge is accessed anonymously; private repositories, \
             pushing, and pull requests will all fail",
            config.forge.token_env
        );
    }

    let forge = ForgeSpec {
        url: config.forge_url().to_string(),
        owner: config.forge.owner.clone(),
        username: config.forge.username.clone(),
        token,
        commit_identity: None,
    };
    let options = Options {
        sync_branch: config.defaults.sync_branch.clone(),
        boundary_file: config.defaults.boundary_file.clone(),
        dry_run,
    };
    let notifiers = build_notifiers(&config);

    if dry_run {
        println!("dry run: nothing will be pushed and no pull request will be touched\n");
    }

    let mut failed = 0;
    for fork in selected {
        let spec = ForkSpec {
            repo: fork.repo.clone(),
            upstream: fork.upstream.clone(),
            branch: fork.branch.clone(),
            upstream_branch: fork.upstream_branch.clone(),
            keep_removed: fork.keep_removed.clone(),
        };
        // The one-shot path has no stop switch of its own: Ctrl-C already ends
        // the process, and every run rebuilds its branch from scratch, so there
        // is nothing a graceful stop would tidy up.
        match runner::sync_fork(&forge, &spec, &options, &weir::git::Cancel::new()) {
            Ok(report) => {
                for line in &report.lines {
                    println!("{}: {line}", fork.repo);
                }
                notify::announce(
                    &notifiers,
                    &notify::summarise(&fork.repo, &report.sync, report.pr_url.as_deref(), dry_run),
                );
            }
            Err(error) => {
                // One bad fork must not stop the others; a weekly run that dies
                // on the first repository silently stops syncing the rest.
                eprintln!("{}: FAILED: {error:#}", fork.repo);
                // And say so out loud. A failure is the outcome most worth
                // hearing about from an unattended run, and stderr on a
                // scheduler nobody reads is the same as silence.
                notify::announce(
                    &notifiers,
                    &format!("❌ {}: sync failed — {error:#}", fork.repo),
                );
                failed += 1;
            }
        }
    }

    anyhow::ensure!(failed == 0, "{failed} fork(s) failed");
    Ok(())
}

/// Builds every configured channel, skipping any whose secrets are absent.
///
/// A missing token is a warning rather than an error: notifications are a
/// courtesy, and refusing to sync because nobody can be told would be the wrong
/// trade every time.
fn build_notifiers(config: &Config) -> Vec<Box<dyn Notifier>> {
    let mut notifiers: Vec<Box<dyn Notifier>> = Vec::new();
    for channel in &config.notify {
        match channel {
            Notify::Telegram {
                token_env,
                chat_env,
            } => match (env_value(token_env), env_value(chat_env)) {
                (Some(token), Some(chat)) => match notify::telegram::Telegram::new(token, chat) {
                    Ok(telegram) => notifiers.push(Box::new(telegram)),
                    Err(error) => eprintln!("note: telegram is configured but unusable: {error:#}"),
                },
                _ => eprintln!(
                    "note: telegram is configured but {token_env} or {chat_env} is unset; \
                     no messages will be sent"
                ),
            },
        }
    }
    notifiers
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
