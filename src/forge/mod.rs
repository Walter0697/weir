//! Talking to the forge, and deciding what to say.
//!
//! The interesting half is [`describe`], which turns a finished sync into the
//! title and body of a pull request. It is a pure function so it can be tested
//! without a forge; everything below it is thin HTTP.
//!
//! The trait exists so a second forge is a new file rather than a rewrite.
//! Gitea and Forgejo answer the same API and share one implementation.

pub mod gitea;

use crate::sync::{Built, Merge};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
}

/// What a sync wants said about it.
#[derive(Debug, PartialEq, Eq)]
pub struct Description {
    pub title: String,
    pub body: String,
}

pub trait Forge {
    /// The open pull request whose head is `head`, if there is one.
    fn find_open(&self, repo: &str, head: &str) -> Result<Option<PullRequest>>;
    fn create(&self, repo: &str, head: &str, base: &str, what: &Description)
        -> Result<PullRequest>;
    /// Refreshes an existing pull request. The force-push already moved its
    /// head; this makes the title and body describe *this* run.
    fn update(&self, repo: &str, number: u64, what: &Description) -> Result<()>;
    fn close(&self, repo: &str, number: u64) -> Result<()>;
}

/// Writes the pull request for a finished sync.
pub fn describe(built: &Built, upstream: &str, upstream_branch: &str, base: &str) -> Description {
    let conflicts = match &built.merge {
        Merge::Clean => Vec::new(),
        Merge::Conflicted { paths } => paths.clone(),
    };

    let title = if conflicts.is_empty() {
        format!("Sync with upstream {upstream_branch}")
    } else {
        format!("Sync with upstream {upstream_branch} (conflicts)")
    };

    let mut body = vec![
        format!("Automated upstream sync from {upstream} ({upstream_branch})."),
        format!(
            "{} new upstream commit{}.",
            built.delta.count,
            if built.delta.count == 1 { "" } else { "s" }
        ),
    ];

    if !built.removed.is_empty() {
        body.push(String::new());
        body.push(
            "Kept removed, because this fork does not carry them:".to_string(),
        );
        body.push(String::new());
        body.extend(built.removed.iter().map(|p| format!("- `{p}`")));
    }

    if conflicts.is_empty() {
        body.push(String::new());
        body.push("Merged cleanly; safe to merge from the UI.".to_string());
    } else {
        body.push(String::new());
        body.push(
            "This merge conflicts, so the branch is the **bare upstream tip** rather than a \
             merge commit — the merge button stays blocked on purpose. Resolve it locally:"
                .to_string(),
        );
        body.push(String::new());
        body.push("```".to_string());
        body.push("git fetch origin upstream-sync".to_string());
        // Note the direction. On conflict the branch carries none of the fork's
        // commits, so merging it into the base branch would present every
        // fork-owned change as a deletion.
        body.push("git checkout -B upstream-sync origin/upstream-sync".to_string());
        body.push(format!("git merge origin/{base}"));
        body.push("# resolve, commit, then:".to_string());
        body.push("git push origin upstream-sync".to_string());
        body.push("```".to_string());
        body.push(String::new());
        body.push("Conflicting paths:".to_string());
        body.push(String::new());
        body.extend(conflicts.iter().map(|p| format!("- `{p}`")));
        body.push(String::new());
        body.push(
            "This pull request closes itself on the next run once the base branch carries \
             the upstream commits."
                .to_string(),
        );
    }

    body.push(String::new());
    body.push(format!("Upstream boundary: `{}`", built.upstream_sha));

    Description {
        title,
        body: body.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{Basis, Delta};

    fn built(count: usize, merge: Merge, removed: &[&str]) -> Built {
        Built {
            delta: Delta {
                count,
                basis: Basis::Recorded("a".repeat(40)),
            },
            merge,
            removed: removed.iter().map(|s| s.to_string()).collect(),
            upstream_sha: "b".repeat(40),
            tip: "c".repeat(40),
        }
    }

    fn conflicted(paths: &[&str]) -> Merge {
        Merge::Conflicted {
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_clean_sync_says_it_is_safe_to_merge() {
        let what = describe(
            &built(51, Merge::Clean, &[]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
        );
        assert_eq!(what.title, "Sync with upstream main");
        assert!(what.body.contains("51 new upstream commits."), "{}", what.body);
        assert!(what.body.contains("safe to merge"), "{}", what.body);
    }

    #[test]
    fn a_conflicting_sync_says_so_in_the_title_so_it_is_visible_in_a_list() {
        let what = describe(
            &built(51, conflicted(&["src/app.rs"]), &[]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
        );
        assert_eq!(what.title, "Sync with upstream main (conflicts)");
        assert!(what.body.contains("- `src/app.rs`"), "{}", what.body);
    }

    /// Getting this backwards discards every fork-owned change, so the
    /// instructions must never drift.
    #[test]
    fn the_resolution_merges_the_base_branch_into_the_sync_branch() {
        let what = describe(
            &built(1, conflicted(&["x"]), &[]),
            "https://example.invalid/up.git",
            "main",
            "canary",
        );
        assert!(
            what.body.contains("git merge origin/canary"),
            "the base branch is merged in: {}",
            what.body
        );
        assert!(
            !what.body.contains("git merge origin/upstream-sync\n"),
            "never the other way: {}",
            what.body
        );
    }

    #[test]
    fn the_base_branch_is_not_assumed_to_be_main() {
        let what = describe(
            &built(3, conflicted(&["x"]), &[]),
            "https://github.com/Dokploy/dokploy.git",
            "canary",
            "canary",
        );
        assert_eq!(what.title, "Sync with upstream canary (conflicts)");
    }

    #[test]
    fn removals_are_named_rather_than_done_silently() {
        let what = describe(
            &built(2, Merge::Clean, &[".github/workflows/rust-release.yml"]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
        );
        assert!(
            what.body.contains("- `.github/workflows/rust-release.yml`"),
            "{}",
            what.body
        );
        assert!(what.body.contains("does not carry them"), "{}", what.body);
    }

    #[test]
    fn one_commit_is_not_reported_as_one_commits() {
        let what = describe(
            &built(1, Merge::Clean, &[]),
            "https://example.invalid/up.git",
            "main",
            "main",
        );
        assert!(what.body.contains("1 new upstream commit."), "{}", what.body);
    }

    /// The boundary is in the body so a human reading the pull request can see
    /// which upstream commit it carries without cloning anything.
    #[test]
    fn the_boundary_is_stated_in_the_body() {
        let what = describe(
            &built(1, Merge::Clean, &[]),
            "https://example.invalid/up.git",
            "main",
            "main",
        );
        assert!(what.body.contains(&"b".repeat(40)), "{}", what.body);
    }
}
