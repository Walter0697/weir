//! Running one fork's sync, for whoever asked.
//!
//! This used to live in the CLI and print as it went, which meant the only way
//! to find out what happened was to be watching a terminal. It returns a report
//! instead: the command prints it, the server stores it, and both see exactly
//! the same thing.

use crate::forge::{self, Forge};
use crate::git::{Cancel, Credential, Git};
use crate::sync::{self, Merge, Plan, Sync};
use anyhow::{Context, Result};
use std::sync::Arc;

/// One fork, resolved from whichever configuration source is in charge.
#[derive(Debug, Clone)]
pub struct ForkSpec {
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
}

impl ForkSpec {
    pub fn upstream_branch(&self) -> &str {
        self.upstream_branch.as_deref().unwrap_or(&self.branch)
    }
}

/// Where the forge is and how to reach it.
#[derive(Debug, Clone)]
pub struct ForgeSpec {
    pub url: String,
    pub owner: String,
    pub username: Option<String>,
    pub token: Option<String>,
    /// Name and email for the commits a sync writes.
    ///
    /// Forges match commits to accounts by email, so leaving this unset gives a
    /// bare author string with no avatar and nothing to click. Setting it to
    /// the machine account's own address makes a sync look like that account
    /// did it, which is true.
    pub commit_identity: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub sync_branch: String,
    pub boundary_file: String,
    pub dry_run: bool,
}

impl Options {
    pub fn one_shot(sync_branch: String, boundary_file: String, dry_run: bool) -> Self {
        Self {
            sync_branch,
            boundary_file,
            dry_run,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Clean,
    Conflicts(usize),
}

impl Outcome {
    /// A short word for a database column or a status column in a table.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::UpToDate => "up to date",
            Outcome::Clean => "clean",
            Outcome::Conflicts(_) => "conflicts",
        }
    }
}

pub struct Report {
    /// Everything the run would have printed, in order.
    pub lines: Vec<String>,
    pub outcome: Outcome,
    pub pr_url: Option<String>,
    /// Kept so a notification can describe the run without re-deriving it.
    pub sync: Sync,
}

impl Report {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Clones the fork, builds the sync branch, pushes it, and reconciles the pull
/// request. Everything it would have said comes back in [`Report::lines`].
pub fn sync_fork(
    forge_spec: &ForgeSpec,
    fork: &ForkSpec,
    options: &Options,
    cancel: &Cancel,
) -> Result<Report> {
    let mut lines = Vec::new();
    let mut say = |line: String| lines.push(line);

    let credential = forge_spec
        .token
        .as_deref()
        .map(|t| Credential::new(t).map(Arc::new))
        .transpose()?;
    let forge = forge_spec
        .token
        .as_deref()
        .map(|t| forge::gitea::Gitea::new(&forge_spec.url, &forge_spec.owner, t))
        .transpose()?;

    let workspace = tempfile::Builder::new()
        .prefix("weir-")
        .tempdir()
        .context("creating a workspace")?;
    let checkout = workspace.path().join(&fork.repo);

    let url = clone_url(forge_spec, &fork.repo);
    let mut git = Git::clone_repo(&url, &fork.branch, &checkout, credential, cancel.clone())?;
    if let Some((name, email)) = &forge_spec.commit_identity {
        git = git.with_identity(crate::git::Identity {
            name: name.clone(),
            email: email.clone(),
        });
    }

    let plan = Plan {
        base_branch: fork.branch.clone(),
        upstream_branch: fork.upstream_branch().to_string(),
        sync_branch: options.sync_branch.clone(),
        boundary_file: options.boundary_file.clone(),
        keep_removed: fork.keep_removed.clone(),
    };

    let outcome = sync::build(&git, &plan, &fork.upstream)?;
    let mut pr_url = None;
    let head = &options.sync_branch;

    // The last point at which stopping costs nothing at all. Past here the
    // branch is pushed, which is harmless in itself — every run rebuilds it
    // from scratch — but it is worth taking the free exit while it is free.
    cancel.check()?;

    let result = match &outcome {
        Sync::UpToDate { delta } => {
            say(format!(
                "up to date on {} (counted from {})",
                fork.branch,
                describe_basis(&delta.basis)
            ));
            // A pull request left open was resolved by merging locally, which
            // never closes it through the API. Retire it here.
            match (&forge, options.dry_run) {
                (None, _) => say("no token, so any open pull request was left alone".into()),
                (Some(forge), dry) => match forge.find_open(&fork.repo, head)? {
                    None => say("no open sync pull request".into()),
                    Some(pr) if dry => {
                        say(format!("would close stale PR #{} (dry run)", pr.number))
                    }
                    Some(pr) => {
                        forge.close(&fork.repo, pr.number)?;
                        say(format!("closed stale PR #{}", pr.number));
                    }
                },
            }
            Outcome::UpToDate
        }
        Sync::Built(built) => {
            say(format!(
                "{} new upstream commit(s) on {} (counted from {})",
                built.delta.count,
                fork.upstream_branch(),
                describe_basis(&built.delta.basis)
            ));
            let result = match &built.merge {
                Merge::Clean => {
                    say("merged cleanly".into());
                    Outcome::Clean
                }
                Merge::Conflicted { paths } => {
                    say(format!(
                        "CONFLICTS in {} path(s); the branch is upstream's tip and the \
                         pull request will not be mergeable",
                        paths.len()
                    ));
                    for path in paths {
                        say(format!("  {path}"));
                    }
                    Outcome::Conflicts(paths.len())
                }
            };
            for removed in &built.removed {
                say(format!(
                    "kept removed: {} ({})",
                    removed.path,
                    match removed.upstream_commits.len() {
                        0 => "unchanged upstream since the last sync".to_string(),
                        1 => "1 upstream commit discarded with it".to_string(),
                        n => format!("{n} upstream commits discarded with it"),
                    }
                ));
            }
            say(format!("boundary {}", built.upstream_sha));

            if options.dry_run {
                say(format!(
                    "would force-push {} at {} (dry run)",
                    options.sync_branch, built.tip
                ));
            } else {
                git.force_push("origin", &options.sync_branch)?;
                say(format!("pushed {} at {}", options.sync_branch, built.tip));
            }

            let what = forge::describe(
                built,
                &fork.upstream,
                fork.upstream_branch(),
                &fork.branch,
                head,
            );
            match (&forge, options.dry_run) {
                (None, _) => say("no token, so the pull request was left alone".into()),
                (Some(forge), dry) => match (forge.find_open(&fork.repo, head)?, dry) {
                    (Some(pr), true) => {
                        say(format!(
                            "would refresh PR #{} — {:?} (dry run)",
                            pr.number, what.title
                        ));
                        pr_url = Some(pr.url);
                    }
                    (Some(pr), false) => {
                        forge.update(&fork.repo, pr.number, &what)?;
                        say(format!("refreshed PR #{} {}", pr.number, pr.url));
                        pr_url = Some(pr.url);
                    }
                    (None, true) => say(format!(
                        "would open a pull request — {:?} (dry run)",
                        what.title
                    )),
                    (None, false) => {
                        let pr = forge.create(&fork.repo, head, &fork.branch, &what)?;
                        say(format!("opened PR #{} {}", pr.number, pr.url));
                        pr_url = Some(pr.url);
                    }
                },
            }
            result
        }
    };

    Ok(Report {
        lines,
        outcome: result,
        pr_url,
        sync: outcome,
    })
}

pub fn describe_basis(basis: &crate::boundary::Basis) -> String {
    match basis {
        crate::boundary::Basis::Recorded(sha) => {
            format!("the recorded boundary {}", &sha[..sha.len().min(12)])
        }
        crate::boundary::Basis::Ancestry => "ancestry, no boundary recorded yet".to_string(),
    }
}

/// The username is not a secret and may go in the URL; the token is supplied
/// separately through `GIT_ASKPASS` so it never reaches a command line.
pub fn clone_url(forge: &ForgeSpec, repo: &str) -> String {
    let base = forge.url.trim_end_matches('/');
    match &forge.username {
        Some(user) => match base.split_once("://") {
            Some((scheme, host)) => format!("{scheme}://{user}@{host}/{}/{repo}.git", forge.owner),
            None => format!("{base}/{}/{repo}.git", forge.owner),
        },
        None => format!("{base}/{}/{repo}.git", forge.owner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forge(username: Option<&str>) -> ForgeSpec {
        ForgeSpec {
            url: "https://forge.example/".to_string(),
            owner: "org".to_string(),
            username: username.map(str::to_string),
            token: None,
            commit_identity: None,
        }
    }

    #[test]
    fn a_clone_url_carries_the_username_but_never_the_token() {
        let url = clone_url(&forge(Some("weir-bot")), "codex");
        assert_eq!(url, "https://weir-bot@forge.example/org/codex.git");
        assert!(!url.contains(':') || !url.contains("@forge.example/org/codex.git:"));
    }

    #[test]
    fn without_a_username_the_url_is_left_plain() {
        assert_eq!(
            clone_url(&forge(None), "codex"),
            "https://forge.example/org/codex.git"
        );
    }

    #[test]
    fn the_upstream_branch_defaults_to_the_one_we_target() {
        let fork = ForkSpec {
            repo: "codex".into(),
            upstream: "https://example.invalid/x.git".into(),
            branch: "canary".into(),
            upstream_branch: None,
            keep_removed: Vec::new(),
        };
        assert_eq!(fork.upstream_branch(), "canary");
    }

    #[test]
    fn outcomes_have_a_short_label_for_a_status_column() {
        assert_eq!(Outcome::UpToDate.label(), "up to date");
        assert_eq!(Outcome::Clean.label(), "clean");
        assert_eq!(Outcome::Conflicts(3).label(), "conflicts");
    }
}
