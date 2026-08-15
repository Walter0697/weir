//! Telling someone a sync happened.
//!
//! A scheduled sync is invisible unless it says something. Nobody goes looking
//! at a forge on the off-chance a weekly job ran, so the run has to come to
//! them.
//!
//! Everything here is **best effort**. A notification is a courtesy, and a
//! missing token or an outage at the other end must never turn a successful
//! sync into a failed one — the work is already done and pushed by the time
//! anything is sent.

pub mod telegram;

use crate::sync::{Merge, Sync};
use anyhow::Result;

pub trait Notifier {
    /// Sends one line about one fork. Errors are the caller's to swallow.
    fn send(&self, message: &str) -> Result<()>;
    /// For log lines, so a failure says which channel gave up.
    fn name(&self) -> &'static str;
}

/// Sends to every configured channel, reporting failures without raising them.
pub fn announce(notifiers: &[Box<dyn Notifier>], message: &str) {
    for notifier in notifiers {
        if let Err(error) = notifier.send(message) {
            eprintln!(
                "note: {} notification failed, continuing: {error:#}",
                notifier.name()
            );
        }
    }
}

/// One line describing what a fork's sync did.
///
/// Written to be legible on a phone: what happened, to what, and where to look.
/// The pull request URL is the whole point — a notification that does not say
/// where to go makes you find it yourself.
pub fn summarise(repo: &str, outcome: &Sync, pr_url: Option<&str>, dry_run: bool) -> String {
    let prefix = if dry_run { "🔎 (dry run) " } else { "" };
    match outcome {
        Sync::UpToDate { .. } => {
            format!("{prefix}✅ {repo}: already level with upstream, nothing to sync")
        }
        Sync::Built(built) => {
            let commits = format!(
                "{} new upstream commit{}",
                built.delta.count,
                if built.delta.count == 1 { "" } else { "s" }
            );
            let head = match &built.merge {
                Merge::Clean => format!("🔄 {repo}: {commits}, merged cleanly"),
                Merge::Conflicted { paths } => format!(
                    "⚠️ {repo}: {commits}, {} conflicting path{} — resolve locally",
                    paths.len(),
                    if paths.len() == 1 { "" } else { "s" }
                ),
            };
            let mut line = format!("{prefix}{head}");
            // Discarded upstream work is worth surfacing here too. It is the
            // one thing a sync throws away without being asked each time.
            let discarded: usize = built.removed.iter().map(|r| r.upstream_commits.len()).sum();
            if discarded > 0 {
                line.push_str(&format!(
                    "\n{discarded} upstream commit(s) discarded with kept-removed paths"
                ));
            }
            if let Some(url) = pr_url {
                line.push_str(&format!("\n{url}"));
            }
            line
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{Basis, Delta};
    use crate::sync::{Built, Removed};

    fn built(count: usize, merge: Merge, discarded: &[&str]) -> Sync {
        Sync::Built(Built {
            delta: Delta {
                count,
                basis: Basis::Ancestry,
            },
            merge,
            removed: if discarded.is_empty() {
                Vec::new()
            } else {
                vec![Removed {
                    path: "ci/dropped.yml".to_string(),
                    upstream_commits: discarded.iter().map(|s| s.to_string()).collect(),
                }]
            },
            upstream_sha: "b".repeat(40),
            tip: "c".repeat(40),
        })
    }

    #[test]
    fn a_clean_sync_says_so_and_links_the_pull_request() {
        let message = summarise(
            "codex",
            &built(51, Merge::Clean, &[]),
            Some("https://forge.example/org/codex/pulls/15"),
            false,
        );
        assert!(message.contains("51 new upstream commits"), "{message}");
        assert!(message.contains("merged cleanly"), "{message}");
        assert!(message.contains("pulls/15"), "{message}");
    }

    #[test]
    fn a_conflicting_sync_counts_the_paths_and_says_it_needs_a_person() {
        let message = summarise(
            "codex",
            &built(
                51,
                Merge::Conflicted {
                    paths: vec!["a".to_string(), "b".to_string()],
                },
                &[],
            ),
            None,
            false,
        );
        assert!(message.contains("2 conflicting paths"), "{message}");
        assert!(message.contains("resolve locally"), "{message}");
    }

    #[test]
    fn nothing_new_upstream_is_still_worth_saying() {
        let message = summarise(
            "dokploy",
            &Sync::UpToDate {
                delta: Delta {
                    count: 0,
                    basis: Basis::Ancestry,
                },
            },
            None,
            false,
        );
        assert!(message.contains("already level"), "{message}");
    }

    #[test]
    fn one_commit_is_not_reported_as_one_commits() {
        let message = summarise("codex", &built(1, Merge::Clean, &[]), None, false);
        assert!(message.contains("1 new upstream commit,"), "{message}");
    }

    #[test]
    fn discarded_upstream_work_is_surfaced_rather_than_left_in_the_pull_request() {
        let message = summarise(
            "codex",
            &built(4, Merge::Clean, &["abc123 something upstream did"]),
            None,
            false,
        );
        assert!(
            message.contains("1 upstream commit(s) discarded"),
            "{message}"
        );
    }

    #[test]
    fn a_dry_run_is_marked_so_nobody_goes_looking_for_a_pull_request() {
        let message = summarise("codex", &built(3, Merge::Clean, &[]), None, true);
        assert!(message.contains("dry run"), "{message}");
    }
}
