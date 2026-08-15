//! A thin wrapper over the `git` CLI.
//!
//! Deliberately the CLI rather than a library: this tool's whole job is to
//! produce a merge that a human will later finish by hand, so it must get
//! exactly the merge that `git merge` gives them — same strategy, same renames,
//! same merge drivers from their `.gitattributes`. A library reimplementation
//! that differs even slightly would produce conflicts the human cannot
//! reproduce locally.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// How to answer git's password prompt, without the secret ever reaching a
/// command line.
///
/// Putting a token in the clone URL — `https://user:token@host/…` — leaks it
/// into the process argument list, which is world-readable on Linux, and into
/// any git error that echoes the remote. `GIT_ASKPASS` keeps it in the
/// environment of the child process instead, which only the same user can read.
pub struct Credential {
    /// Kept so the directory outlives the script inside it.
    _dir: tempfile::TempDir,
    script: PathBuf,
    token: String,
}

impl Credential {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("weir-askpass")
            .tempdir()
            .context("creating a private directory for the askpass helper")?;
        let script = dir.path().join("askpass");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$WEIR_ASKPASS_TOKEN\"\n")
            .context("writing the askpass helper")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .context("restricting the askpass helper")?;
        }
        Ok(Self {
            _dir: dir,
            script,
            token: token.into(),
        })
    }
}

pub struct Git {
    root: PathBuf,
    credential: Option<Arc<Credential>>,
    identity: Identity,
}

/// Who the merge and boundary commits are attributed to.
#[derive(Clone)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            name: "weir[bot]".to_string(),
            email: "weir@users.noreply.localhost".to_string(),
        }
    }
}

/// What a git invocation produced. `status` is kept so callers can tell a
/// meaningful non-zero exit (a conflict, a missing object) from a real failure.
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

fn command(root: Option<&Path>, credential: Option<&Credential>, identity: &Identity) -> Command {
    let mut cmd = Command::new("git");
    if let Some(root) = root {
        cmd.arg("-C").arg(root);
    }
    // Identity is passed per invocation rather than written into the clone's
    // config, so a repository that already has one is never modified.
    cmd.arg("-c")
        .arg(format!("user.name={}", identity.name))
        .arg("-c")
        .arg(format!("user.email={}", identity.email))
        .arg("-c")
        .arg("commit.gpgsign=false");
    // Never block waiting for a human. Without this a missing credential hangs
    // a scheduled run until it is killed.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(cred) = credential {
        cmd.env("GIT_ASKPASS", &cred.script);
        cmd.env("WEIR_ASKPASS_TOKEN", &cred.token);
    }
    cmd
}

impl Git {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            credential: None,
            identity: Identity::default(),
        }
    }

    pub fn with_credential(mut self, credential: Option<Arc<Credential>>) -> Self {
        self.credential = credential;
        self
    }

    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Clones `url` at `branch` into `dest`.
    ///
    /// A blobless partial clone: the sync needs full history to count commits
    /// and to merge, but not every historical file version. On a large upstream
    /// that is the difference between a fast weekly run and a slow one.
    pub fn clone_repo(
        url: &str,
        branch: &str,
        dest: &Path,
        credential: Option<Arc<Credential>>,
    ) -> Result<Self> {
        let identity = Identity::default();
        let out = command(None, credential.as_deref(), &identity)
            .args(["clone", "--quiet", "--filter=blob:none", "--branch", branch])
            .arg(url)
            .arg(dest)
            .output()
            .context("running `git clone`")?;
        if !out.status.success() {
            bail!(
                "cloning {url} at {branch} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(Self::new(dest).with_credential(credential))
    }

    /// Runs git and returns its output whatever the exit status.
    pub fn try_run(&self, args: &[&str]) -> Result<Output> {
        let out = command(Some(&self.root), self.credential.as_deref(), &self.identity)
            .args(args)
            .output()
            .with_context(|| format!("running `git {}`", args.join(" ")))?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        })
    }

    /// Runs git and fails if it exits non-zero, reporting what git said.
    ///
    /// The stderr is carried into the error on purpose: a sync that dies should
    /// say which repository, which command, and what git complained about,
    /// rather than leaving an exit code in a log.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.try_run(args)?;
        if !out.ok() {
            bail!(
                "`git {}` failed with status {}: {}",
                args.join(" "),
                out.status,
                if out.stderr.is_empty() {
                    &out.stdout
                } else {
                    &out.stderr
                }
            );
        }
        Ok(out.stdout)
    }

    /// Whether a commit object is present in this repository.
    pub fn has_commit(&self, sha: &str) -> Result<bool> {
        Ok(self
            .try_run(&["cat-file", "-e", &format!("{sha}^{{commit}}")])?
            .ok())
    }

    /// Number of commits reachable from `to` but not from `from`.
    pub fn count_commits(&self, from: &str, to: &str) -> Result<usize> {
        let out = self.run(&["rev-list", "--count", &format!("{from}..{to}")])?;
        out.trim()
            .parse()
            .with_context(|| format!("parsing commit count from {out:?}"))
    }

    /// The contents of a path as of a ref, or `None` when it does not exist there.
    pub fn show_file(&self, at_ref: &str, path: &str) -> Result<Option<String>> {
        let out = self.try_run(&["show", &format!("{at_ref}:{path}")])?;
        Ok(out.ok().then_some(out.stdout))
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        self.run(&["rev-parse", rev])
    }

    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.run(&["remote", "add", name, url])?;
        Ok(())
    }

    /// Commits between two refs that touched a path, newest first, as
    /// `abbrev subject`.
    ///
    /// `--follow` is deliberately not used: it needs a path that exists at the
    /// tip, and these paths are precisely the ones that do not.
    pub fn commits_touching(&self, from: &str, to: &str, path: &str) -> Result<Vec<String>> {
        Ok(self
            .run(&[
                "log",
                "--format=%h %s",
                &format!("{from}..{to}"),
                "--",
                path,
            ])?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn fetch(&self, remote: &str, branch: &str) -> Result<()> {
        self.run(&["fetch", "--quiet", "--filter=blob:none", remote, branch])?;
        Ok(())
    }

    /// Creates or resets `branch` to `start` and checks it out.
    pub fn checkout_new(&self, branch: &str, start: &str) -> Result<()> {
        self.run(&["checkout", "--quiet", "-B", branch, start])?;
        Ok(())
    }

    /// Attempts a merge. Returns whether it succeeded; a failure here is a
    /// conflict, not an error, so it is reported rather than raised.
    pub fn merge(&self, refname: &str) -> Result<bool> {
        Ok(self
            .try_run(&["merge", "--quiet", "--no-edit", "--no-ff", refname])?
            .ok())
    }

    pub fn merge_abort(&self) -> Result<()> {
        // Best effort: if there is no merge in progress this is already the
        // state we want.
        self.try_run(&["merge", "--abort"])?;
        Ok(())
    }

    /// Paths left unresolved by a merge.
    pub fn conflicted_paths(&self) -> Result<Vec<String>> {
        Ok(self
            .run(&["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Whether a path is tracked at the current head.
    pub fn is_tracked(&self, path: &str) -> Result<bool> {
        Ok(!self.run(&["ls-files", "--", path])?.trim().is_empty())
    }

    /// Whether a path exists in the tree of some ref.
    pub fn path_exists_at(&self, at_ref: &str, path: &str) -> Result<bool> {
        Ok(self
            .try_run(&["cat-file", "-e", &format!("{at_ref}:{path}")])?
            .ok())
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        self.run(&["rm", "--quiet", "--force", "--ignore-unmatch", "--", path])?;
        Ok(())
    }

    pub fn add(&self, path: &str) -> Result<()> {
        self.run(&["add", "--", path])?;
        Ok(())
    }

    /// Whether anything is staged. Used to avoid committing nothing, which git
    /// treats as an error.
    pub fn has_staged_changes(&self) -> Result<bool> {
        Ok(!self.try_run(&["diff", "--cached", "--quiet"])?.ok())
    }

    pub fn commit(&self, message: &str) -> Result<()> {
        self.run(&["commit", "--quiet", "-m", message])?;
        Ok(())
    }

    /// Force-updates `branch` on `remote` from the current head.
    pub fn force_push(&self, remote: &str, branch: &str) -> Result<()> {
        self.run(&[
            "push",
            "--quiet",
            "--force",
            remote,
            &format!("HEAD:refs/heads/{branch}"),
        ])?;
        Ok(())
    }
}
