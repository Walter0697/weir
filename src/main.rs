//! Syncs a fork with its upstream and opens a pull request for review.
//!
//! The binary performs one pass and exits. Deciding *when* it runs belongs
//! outside it — a scheduler, a cron, a CI workflow, or the `serve` command once
//! that exists.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use weir::config::{Config, Fork};
use weir::forge::Forge;
use weir::git::{Credential, Git};
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
    let token = std::env::var(&config.forge.token_env)
        .ok()
        .filter(|t| !t.trim().is_empty());
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

    if dry_run {
        println!("dry run: nothing will be pushed and no pull request will be touched\n");
    }

    let mut failed = 0;
    for fork in selected {
        if let Err(error) = sync_one(&config, fork, credential.clone(), forge, dry_run) {
            // One bad fork must not stop the others; a weekly run that dies on
            // the first repository silently stops syncing the rest.
            eprintln!("{}: FAILED: {error:#}", fork.repo);
            failed += 1;
        }
    }

    anyhow::ensure!(failed == 0, "{failed} fork(s) failed");
    Ok(())
}

fn sync_one(
    config: &Config,
    fork: &Fork,
    credential: Option<Arc<Credential>>,
    forge: Option<&dyn Forge>,
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

    match sync::build(&git, &plan, &fork.upstream)? {
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
            for path in &built.removed {
                println!("{}: kept removed: {path}", fork.repo);
            }
            println!("{}: boundary {}", fork.repo, built.upstream_sha);

            if dry_run {
                println!(
                    "{}: would force-push {} at {} (dry run)",
                    fork.repo, plan.sync_branch, built.tip
                );
            } else {
                git.force_push("origin", &plan.sync_branch)?;
                println!("{}: pushed {} at {}", fork.repo, plan.sync_branch, built.tip);
            }

            reconcile_pr(config, fork, &built, forge, dry_run)?;
        }
    }

    Ok(())
}

fn reconcile_pr(
    config: &Config,
    fork: &Fork,
    built: &weir::sync::Built,
    forge: Option<&dyn Forge>,
    dry_run: bool,
) -> Result<()> {
    let head = &config.defaults.sync_branch;
    let what = weir::forge::describe(built, &fork.upstream, fork.upstream_branch(), &fork.branch);

    let Some(forge) = forge else {
        println!("{}: no token, so the pull request was left alone", fork.repo);
        return Ok(());
    };

    // The force-push already moved an existing pull request's head; only the
    // title and body still have to follow *this* run's outcome.
    let existing = forge.find_open(&fork.repo, head)?;
    match (existing, dry_run) {
        (Some(pr), true) => println!(
            "{}: would refresh PR #{} — {:?} (dry run)",
            fork.repo, pr.number, what.title
        ),
        (Some(pr), false) => {
            forge.update(&fork.repo, pr.number, &what)?;
            println!("{}: refreshed PR #{} {}", fork.repo, pr.number, pr.url);
        }
        (None, true) => println!(
            "{}: would open a pull request — {:?} (dry run)",
            fork.repo, what.title
        ),
        (None, false) => {
            let pr = forge.create(&fork.repo, head, &fork.branch, &what)?;
            println!("{}: opened PR #{} {}", fork.repo, pr.number, pr.url);
        }
    }
    Ok(())
}

fn retire_stale(
    fork: &Fork,
    forge: Option<&dyn Forge>,
    head: &str,
    dry_run: bool,
) -> Result<()> {
    let Some(forge) = forge else {
        println!("{}: no token, so any open pull request was left alone", fork.repo);
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
