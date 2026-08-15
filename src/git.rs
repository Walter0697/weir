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

pub struct Git {
    root: PathBuf,
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

impl Git {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs git and returns its output whatever the exit status.
    pub fn try_run(&self, args: &[&str]) -> Result<Output> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
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
    /// The stderr is carried into the error on purpose: a sync that dies
    /// should say which repository, which command, and what git complained
    /// about, rather than leaving an exit code in a log.
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
        Ok(self.try_run(&["cat-file", "-e", &format!("{sha}^{{commit}}")])?.ok())
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
}
