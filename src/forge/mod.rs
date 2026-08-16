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

/// A repository on the forge, as offered when adding a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub name: String,
    pub default_branch: String,
    /// Read-only on the forge. Nothing can be pushed to it, so syncing one
    /// would fail on every run rather than usefully.
    pub archived: bool,
    /// A pull mirror. The forge force-syncs it from upstream on its own
    /// schedule, discarding anything local — so it looks like a perfectly
    /// good fork and is the worst possible thing to sync.
    pub mirror: bool,
    /// Where the repository was migrated from, when the forge recorded it.
    /// Gitea keeps this as `original_url`, which is usually exactly the
    /// upstream you want, so it can be offered rather than typed.
    pub upstream: Option<String>,
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
    /// Repositories under the configured owner, for picking rather than typing.
    fn discover(&self) -> Result<Vec<Discovered>>;
}

/// Writes the pull request for a finished sync.
pub fn describe(
    built: &Built,
    upstream: &str,
    upstream_branch: &str,
    base: &str,
    sync_branch: &str,
) -> Description {
    let conflicts = match &built.merge {
        Merge::Clean => Vec::new(),
        Merge::Conflicted { paths } => paths.clone(),
    };

    // The title says what the pull request is, not how it went. Whether it
    // conflicted is already visible in the body and in the blocked merge
    // button, and a title that changes between runs makes the same pull
    // request look like a different one each week.
    let title = format!("Sync with upstream {upstream_branch}");

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
        body.push("Kept removed, because this fork does not carry them:".to_string());
        body.push(String::new());
        for removed in &built.removed {
            body.push(format!("- `{}`", removed.path));
            // What was discarded is spelled out. A path is usually dropped to be
            // rid of one feature, and upstream may put something worth having
            // inside the same file later; nothing can judge that automatically,
            // so the commits are listed and the reading is left to a person.
            body.extend(
                summarise(&removed.upstream_commits)
                    .iter()
                    .map(|line| format!("  - {line}")),
            );
        }
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
        body.push(format!("git fetch origin {sync_branch}"));
        // Note the direction. On conflict the branch carries none of the fork's
        // commits, so merging it into the base branch would present every
        // fork-owned change as a deletion.
        body.push(format!(
            "git checkout -B {sync_branch} origin/{sync_branch}"
        ));
        body.push(format!("git merge origin/{base}"));
        body.push("# resolve, commit, then:".to_string());
        body.push(format!("git push origin {sync_branch}"));
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

/// At most a handful of commit lines, with a count for the rest.
///
/// A path dropped years ago can accumulate hundreds of upstream commits, and a
/// pull request body that opens with two hundred lines of them is one nobody
/// reads.
const SUMMARY_LIMIT: usize = 5;

fn summarise(commits: &[String]) -> Vec<String> {
    if commits.is_empty() {
        return vec!["no upstream changes since the last sync".to_string()];
    }
    let mut lines: Vec<String> = commits.iter().take(SUMMARY_LIMIT).cloned().collect();
    if commits.len() > SUMMARY_LIMIT {
        lines.push(format!(
            "…and {} more upstream commit(s) discarded with this path",
            commits.len() - SUMMARY_LIMIT
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{Basis, Delta};
    use crate::sync::Removed;

    fn removed(path: &str, commits: &[&str]) -> Removed {
        Removed {
            path: path.to_string(),
            upstream_commits: commits.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn built(count: usize, merge: Merge, removed: &[&str]) -> Built {
        Built {
            delta: Delta {
                count,
                basis: Basis::Recorded("a".repeat(40)),
            },
            merge,
            removed: removed
                .iter()
                .map(|s| Removed {
                    path: s.to_string(),
                    upstream_commits: Vec::new(),
                })
                .collect(),
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
            "upstream-sync",
        );
        assert_eq!(what.title, "Sync with upstream main");
        assert!(
            what.body.contains("51 new upstream commits."),
            "{}",
            what.body
        );
        assert!(what.body.contains("safe to merge"), "{}", what.body);
    }

    /// The title is the same either way, so a refreshed pull request does not
    /// look like a new one; the body carries the outcome.
    #[test]
    fn the_title_does_not_change_when_a_sync_conflicts() {
        let clean = describe(
            &built(51, Merge::Clean, &[]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
            "upstream-sync",
        );
        let messy = describe(
            &built(51, conflicted(&["src/app.rs"]), &[]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
            "upstream-sync",
        );
        assert_eq!(clean.title, messy.title);
        assert_eq!(messy.title, "Sync with upstream main");
        assert!(messy.body.contains("- `src/app.rs`"), "{}", messy.body);
        assert!(messy.body.contains("bare upstream tip"), "{}", messy.body);
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
            "upstream-sync",
        );
        assert!(
            what.body.contains("git merge origin/canary"),
            "the base branch is merged in: {}",
            what.body
        );
        assert!(
            !what.body.contains("git merge origin/upstream-sync"),
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
            "upstream-sync",
        );
        assert_eq!(what.title, "Sync with upstream canary");
        assert!(
            what.body.contains("git merge origin/canary"),
            "{}",
            what.body
        );
    }

    /// The branch name is configurable, so the instructions must not hardcode it.
    #[test]
    fn the_resolution_uses_the_configured_sync_branch_name() {
        let what = describe(
            &built(1, conflicted(&["x"]), &[]),
            "https://example.invalid/up.git",
            "main",
            "main",
            "vendor-sync",
        );
        assert!(
            what.body.contains("git fetch origin vendor-sync"),
            "{}",
            what.body
        );
        assert!(
            what.body.contains("git push origin vendor-sync"),
            "{}",
            what.body
        );
        assert!(!what.body.contains("upstream-sync"), "{}", what.body);
    }

    /// Keeping a path removed throws away whatever upstream put in it. The
    /// body has to say so, or the loss is invisible.
    #[test]
    fn the_body_names_the_upstream_commits_a_removal_discards() {
        let mut what = built(2, Merge::Clean, &[]);
        what.removed = vec![removed(
            ".github/workflows/rust-release.yml",
            &[
                "8c3b7b8 ci: replace GitHub workflows",
                "f363ed7 fix(release): signing",
            ],
        )];
        let described = describe(
            &what,
            "https://github.com/openai/codex.git",
            "main",
            "main",
            "upstream-sync",
        );
        assert!(described.body.contains("8c3b7b8"), "{}", described.body);
        assert!(described.body.contains("f363ed7"), "{}", described.body);
    }

    #[test]
    fn a_long_discard_list_is_summarised_rather_than_dumped() {
        let many: Vec<String> = (0..12).map(|i| format!("abc{i:04} commit {i}")).collect();
        let lines = summarise(&many);
        assert_eq!(lines.len(), SUMMARY_LIMIT + 1);
        assert!(lines.last().unwrap().contains("and 7 more"), "{lines:?}");
    }

    #[test]
    fn a_path_upstream_has_not_touched_says_so_rather_than_showing_nothing() {
        assert_eq!(
            summarise(&[]),
            vec!["no upstream changes since the last sync".to_string()]
        );
    }

    #[test]
    fn removals_are_named_rather_than_done_silently() {
        let what = describe(
            &built(2, Merge::Clean, &[".github/workflows/rust-release.yml"]),
            "https://github.com/openai/codex.git",
            "main",
            "main",
            "upstream-sync",
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
            "upstream-sync",
        );
        assert!(
            what.body.contains("1 new upstream commit."),
            "{}",
            what.body
        );
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
            "upstream-sync",
        );
        assert!(what.body.contains(&"b".repeat(40)), "{}", what.body);
    }
}
