//! Syncs a fork with its upstream and opens a pull request for the result.
//!
//! The binary performs one pass and exits. Deciding *when* it runs belongs
//! outside it — a scheduler, a cron, a CI workflow, or the `serve` command once
//! that exists.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use weir::config::{Config, Fork, Notify};
use weir::forge::Forge;
use weir::git::{Credential, Git};
use weir::notify::{self, Notifier};
use weir::sync::{self, Plan, Sync};

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
    /// Sync each fork and report what happened.
    Run {
        /// Only this fork, rather than every one in the config.
        #[arg(long)]
        repo: Option<String>,
        /// Do everything except the parts that cannot be undone: no push, no
        /// pull request. Safe to point at a live forge.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate => validate(&cli.config),
        Command::Run { repo, dry_run } => run(&cli.config, repo.as_deref(), dry_run),
    }
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

    // One token serves both halves: git uses it as the push password, the API
    // uses it as a bearer.
    // Trimmed, because a token routinely arrives with a trailing newline — from
    // a pasted heredoc, a docker `--env-file`, or a mounted secret — and the
    // resulting authentication failure says nothing about why.
    let token = std::env::var(&config.forge.token_env)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if token.is_none() {
        eprintln!(
            "note: {} is unset, so the forge is accessed anonymously; private repositories, \
             pushing, and pull requests will all fail",
            config.forge.token_env
        );
    }

    let credential = token
        .as_deref()
        .map(|t| Credential::new(t).map(Arc::new))
        .transpose()?;
    let forge = token
        .as_deref()
        .map(|t| weir::forge::gitea::Gitea::new(config.forge_url(), &config.forge.owner, t))
        .transpose()?;
    let forge = forge.as_ref().map(|g| g as &dyn Forge);

    let notifiers = build_notifiers(&config);

    if dry_run {
        println!("dry run: nothing will be pushed and no pull request will be touched\n");
    }

    let mut failed = 0;
    for fork in selected {
        if let Err(error) = sync_one(
            &config,
            fork,
            credential.clone(),
            forge,
            &notifiers,
            dry_run,
        ) {
            // One bad fork must not stop the others; a weekly run that dies on
            // the first repository silently stops syncing the rest.
            eprintln!("{}: FAILED: {error:#}", fork.repo);
            failed += 1;
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

fn sync_one(
    config: &Config,
    fork: &Fork,
    credential: Option<Arc<Credential>>,
    forge: Option<&dyn Forge>,
    notifiers: &[Box<dyn Notifier>],
    dry_run: bool,
) -> Result<()> {
    let workspace = tempfile::Builder::new()
        .prefix("weir-")
        .tempdir()
        .context("creating a workspace")?;
    let checkout = workspace.path().join(&fork.repo);

    let url = clone_url(config, &fork.repo);
    let git = Git::clone_repo(&url, &fork.branch, &checkout, credential)?;

    let plan = Plan {
        base_branch: fork.branch.clone(),
        upstream_branch: fork.upstream_branch().to_string(),
        sync_branch: config.defaults.sync_branch.clone(),
        boundary_file: config.defaults.boundary_file.clone(),
        keep_removed: fork.keep_removed.clone(),
    };

    let outcome = sync::build(&git, &plan, &fork.upstream)?;
    let mut pr_url = None;

    match &outcome {
        Sync::UpToDate { delta } => {
            println!(
                "{}: up to date on {} (counted from {})",
                fork.repo,
                fork.branch,
                describe(&delta.basis)
            );
            // Nothing new upstream. A pull request that is still open was
            // resolved by merging locally, which never closes it through the
            // API, so retire it here.
            retire_stale(fork, forge, &config.defaults.sync_branch, dry_run)?;
        }
        Sync::Built(built) => {
            println!(
                "{}: {} new upstream commit(s) on {} (counted from {})",
                fork.repo,
                built.delta.count,
                fork.upstream_branch(),
                describe(&built.delta.basis)
            );
            match &built.merge {
                sync::Merge::Clean => println!("{}: merged cleanly", fork.repo),
                sync::Merge::Conflicted { paths } => {
                    println!(
                        "{}: CONFLICTS in {} path(s); the branch is upstream's tip \
                         and the pull request will not be mergeable",
                        fork.repo,
                        paths.len()
                    );
                    for path in paths {
                        println!("{}:   {path}", fork.repo);
                    }
                }
            }
            for removed in &built.removed {
                // Say what went with it. Keeping a path removed discards every
                // upstream change inside it, and this count is the only warning
                // anyone gets that something worth having may be in there.
                println!(
                    "{}: kept removed: {} ({})",
                    fork.repo,
                    removed.path,
                    match removed.upstream_commits.len() {
                        0 => "unchanged upstream since the last sync".to_string(),
                        1 => "1 upstream commit discarded with it".to_string(),
                        n => format!("{n} upstream commits discarded with it"),
                    }
                );
            }
            println!("{}: boundary {}", fork.repo, built.upstream_sha);

            if dry_run {
                println!(
                    "{}: would force-push {} at {} (dry run)",
                    fork.repo, plan.sync_branch, built.tip
                );
            } else {
                git.force_push("origin", &plan.sync_branch)?;
                println!(
                    "{}: pushed {} at {}",
                    fork.repo, plan.sync_branch, built.tip
                );
            }

            pr_url = reconcile_pr(config, fork, built, forge, dry_run)?;
        }
    }

    // Last, and never fatal. The sync is already pushed by this point; a
    // Telegram outage must not turn a completed run into a failed one.
    notify::announce(
        notifiers,
        &notify::summarise(&fork.repo, &outcome, pr_url.as_deref(), dry_run),
    );

    Ok(())
}

fn reconcile_pr(
    config: &Config,
    fork: &Fork,
    built: &weir::sync::Built,
    forge: Option<&dyn Forge>,
    dry_run: bool,
) -> Result<Option<String>> {
    let head = &config.defaults.sync_branch;
    let what = weir::forge::describe(
        built,
        &fork.upstream,
        fork.upstream_branch(),
        &fork.branch,
        head,
    );

    let Some(forge) = forge else {
        println!(
            "{}: no token, so the pull request was left alone",
            fork.repo
        );
        return Ok(None);
    };

    // The force-push already moved an existing pull request's head; only the
    // title and body still have to follow *this* run's outcome.
    let existing = forge.find_open(&fork.repo, head)?;
    Ok(match (existing, dry_run) {
        (Some(pr), true) => {
            println!(
                "{}: would refresh PR #{} — {:?} (dry run)",
                fork.repo, pr.number, what.title
            );
            Some(pr.url)
        }
        (Some(pr), false) => {
            forge.update(&fork.repo, pr.number, &what)?;
            println!("{}: refreshed PR #{} {}", fork.repo, pr.number, pr.url);
            Some(pr.url)
        }
        (None, true) => {
            println!(
                "{}: would open a pull request — {:?} (dry run)",
                fork.repo, what.title
            );
            None
        }
        (None, false) => {
            let pr = forge.create(&fork.repo, head, &fork.branch, &what)?;
            println!("{}: opened PR #{} {}", fork.repo, pr.number, pr.url);
            Some(pr.url)
        }
    })
}

fn retire_stale(fork: &Fork, forge: Option<&dyn Forge>, head: &str, dry_run: bool) -> Result<()> {
    let Some(forge) = forge else {
        println!(
            "{}: no token, so any open pull request was left alone",
            fork.repo
        );
        return Ok(());
    };
    match forge.find_open(&fork.repo, head)? {
        None => println!("{}: no open sync pull request", fork.repo),
        Some(pr) if dry_run => println!(
            "{}: would close stale PR #{} (dry run)",
            fork.repo, pr.number
        ),
        Some(pr) => {
            forge.close(&fork.repo, pr.number)?;
            println!("{}: closed stale PR #{}", fork.repo, pr.number);
        }
    }
    Ok(())
}

fn describe(basis: &weir::boundary::Basis) -> String {
    match basis {
        weir::boundary::Basis::Recorded(sha) => {
            format!("the recorded boundary {}", &sha[..sha.len().min(12)])
        }
        weir::boundary::Basis::Ancestry => "ancestry, no boundary recorded yet".to_string(),
    }
}

fn clone_url(config: &Config, repo: &str) -> String {
    let base = config.forge_url();
    match &config.forge.username {
        // The username is not a secret; the token is supplied separately via
        // GIT_ASKPASS so it never appears in a command line.
        Some(user) => match base.split_once("://") {
            Some((scheme, host)) => {
                format!("{scheme}://{user}@{host}/{}/{repo}.git", config.forge.owner)
            }
            None => format!("{base}/{}/{repo}.git", config.forge.owner),
        },
        None => format!("{base}/{}/{repo}.git", config.forge.owner),
    }
}
