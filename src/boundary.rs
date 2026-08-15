//! Where the fork's content currently sits relative to upstream.
//!
//! # Why this is a file and not ancestry
//!
//! Sync pull requests get squash-merged. After a squash, upstream's commits are
//! not ancestors of our base branch — only their *content* is. So asking git
//! "which upstream commits are unreachable from the fork" answers "all of them,
//! since the fork began", and every count downstream of that is wrong.
//!
//! Measured on the fork that motivated this: ancestry reported **663 new
//! upstream commits when roughly 44 had landed**. That inflates every pull
//! request body, and it means the "nothing new, retire the stale PR" path can
//! never be reached, because the count is never zero.
//!
//! A file is content, so it survives a squash the way a tag, a commit message,
//! and ancestry all do not. It is read from the *base* branch, never from the
//! sync branch, which is what makes closing a pull request without merging it
//! do the right thing: the boundary has not moved, so the next run recomputes
//! the same delta and rebuilds the branch.
//!
//! # Why a bad boundary is an error
//!
//! The shell version this replaces fell back to ancestry whenever the recorded
//! boundary could not be used, for any reason. That is safe for a repository
//! that has never been synced, and actively harmful for every other case: it
//! silently reintroduces the 663 and leaves nothing behind to explain it.
//!
//! So the two cases are separated. *Absent* falls back, because there is
//! genuinely nothing to read. *Present but unusable* fails loudly, because
//! something is wrong that a human needs to look at.

use crate::git::Git;
use anyhow::{bail, Result};

/// What the boundary file on the base branch turned out to contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boundary {
    /// A well-formed commit id.
    Recorded(String),
    /// No boundary file on the base branch. Legitimate for a fork that has not
    /// been synced since this tool existed.
    Absent,
    /// The file is there but does not hold a commit id.
    Unusable { raw: String, reason: String },
}

/// How a delta was arrived at, so callers can report it honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// Counted from the recorded boundary.
    Recorded(String),
    /// No boundary was recorded, so this came from ancestry, which is correct
    /// only while the fork's history has never been squashed.
    Ancestry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub count: usize,
    pub basis: Basis,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Reads the boundary file from `base_ref` without judging it.
pub fn read(git: &Git, base_ref: &str, boundary_file: &str) -> Result<Boundary> {
    let Some(contents) = git.show_file(base_ref, boundary_file)? else {
        return Ok(Boundary::Absent);
    };

    // Written by `printf '%s\n'`, and hand-edited at least once when the forks
    // were seeded, so tolerate surrounding whitespace and a trailing newline.
    let raw = contents.trim().to_string();

    if raw.is_empty() {
        return Ok(Boundary::Unusable {
            raw,
            reason: "the file is empty".to_string(),
        });
    }
    if !is_commit_id(&raw) {
        return Ok(Boundary::Unusable {
            reason: format!("{raw:?} is not a commit id"),
            raw,
        });
    }
    Ok(Boundary::Recorded(raw))
}

/// How many upstream commits have landed since the fork's content was last
/// brought level.
pub fn delta(git: &Git, base_ref: &str, upstream_ref: &str, boundary_file: &str) -> Result<Delta> {
    match read(git, base_ref, boundary_file)? {
        Boundary::Recorded(sha) => {
            if !git.has_commit(&sha)? {
                // Well-formed but not here. Upstream rewrote history, or this
                // clone never fetched far enough back. Falling back to ancestry
                // would quietly produce the inflated count, so refuse instead.
                bail!(
                    "{boundary_file} on {base_ref} records {sha}, which is not in this repository \
                     — upstream may have rewritten history, or the clone is too shallow. \
                     Refusing to fall back to ancestry, which would report every upstream commit \
                     since the fork began."
                );
            }
            Ok(Delta {
                count: git.count_commits(&sha, upstream_ref)?,
                basis: Basis::Recorded(sha),
            })
        }
        Boundary::Absent => Ok(Delta {
            count: git.count_commits(base_ref, upstream_ref)?,
            basis: Basis::Ancestry,
        }),
        Boundary::Unusable { raw, reason } => bail!(
            "{boundary_file} on {base_ref} does not hold a usable commit id: {reason}. \
             Fix or remove the file; an absent boundary falls back to ancestry, \
             but a corrupt one is not guessed at. Contents: {raw:?}"
        ),
    }
}

/// Accepts full and abbreviated object ids. Git will resolve an abbreviation,
/// and the forks were seeded by hand, so refusing short ids would be unkind.
fn is_commit_id(s: &str) -> bool {
    let len = s.len();
    (7..=64).contains(&len) && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Git;
    use tempfile::TempDir;

    const BOUNDARY: &str = ".upstream-sync";

    /// A repository with a `main` branch standing in for the fork and an `up`
    /// branch standing in for the fetched upstream.
    struct Fixture {
        _dir: TempDir,
        git: Git,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let git = Git::new(dir.path());
            git.run(&["init", "--quiet", "--initial-branch=main"]).unwrap();
            git.run(&["config", "user.name", "test"]).unwrap();
            git.run(&["config", "user.email", "test@example.invalid"]).unwrap();
            git.run(&["config", "commit.gpgsign", "false"]).unwrap();
            let fixture = Self { _dir: dir, git };
            fixture.commit("base", "base\n");
            fixture.git.run(&["branch", "up"]).unwrap();
            fixture
        }

        fn commit(&self, name: &str, contents: &str) -> String {
            std::fs::write(self.git.root().join(name), contents).unwrap();
            self.git.run(&["add", "."]).unwrap();
            self.git.run(&["commit", "--quiet", "-m", name]).unwrap();
            self.git.rev_parse("HEAD").unwrap()
        }

        fn checkout(&self, branch: &str) {
            self.git.run(&["checkout", "--quiet", branch]).unwrap();
        }

        /// Adds `n` commits to the upstream branch and returns its new tip.
        fn upstream_commits(&self, n: usize) -> String {
            self.checkout("up");
            let mut tip = String::new();
            for i in 0..n {
                tip = self.commit(&format!("upstream-{i}-{}", rand_suffix()), "u\n");
            }
            self.checkout("main");
            tip
        }

        /// Brings `main` level with `up` the way a squash merge does: the
        /// content arrives, the commits do not become ancestors.
        fn squash_merge_upstream(&self) {
            self.checkout("main");
            self.git.run(&["merge", "--squash", "-q", "up"]).unwrap();
            self.git.run(&["commit", "--quiet", "-m", "Sync with upstream (#7)"]).unwrap();
        }

        fn record_boundary(&self, sha: &str) {
            self.checkout("main");
            std::fs::write(self.git.root().join(BOUNDARY), format!("{sha}\n")).unwrap();
            self.git.run(&["add", BOUNDARY]).unwrap();
            self.git
                .run(&["commit", "--quiet", "-m", "Record upstream sync boundary"])
                .unwrap();
        }

        fn write_boundary_verbatim(&self, contents: &str) {
            self.checkout("main");
            std::fs::write(self.git.root().join(BOUNDARY), contents).unwrap();
            self.git.run(&["add", BOUNDARY]).unwrap();
            self.git.run(&["commit", "--quiet", "-m", "boundary"]).unwrap();
        }

        fn delta(&self) -> Result<Delta> {
            super::delta(&self.git, "main", "up", BOUNDARY)
        }
    }

    fn rand_suffix() -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        N.fetch_add(1, Ordering::Relaxed).to_string()
    }

    #[test]
    fn with_no_boundary_recorded_the_count_comes_from_ancestry() {
        let f = Fixture::new();
        f.upstream_commits(3);
        let delta = f.delta().unwrap();
        assert_eq!(delta.count, 3);
        assert_eq!(delta.basis, Basis::Ancestry);
    }

    /// The regression this whole design exists for.
    ///
    /// After a squash merge the fork's content is level with upstream, but
    /// ancestry still reports every upstream commit as new. The recorded
    /// boundary is what turns that back into the truth.
    #[test]
    fn after_a_squash_merge_ancestry_lies_and_the_boundary_does_not() {
        let f = Fixture::new();
        let upstream_tip = f.upstream_commits(3);
        f.squash_merge_upstream();

        // What the old shell version did: ask ancestry.
        let by_ancestry = f.git.count_commits("main", "up").unwrap();
        assert_eq!(by_ancestry, 3, "ancestry still counts the squashed commits");

        // What this does instead.
        f.record_boundary(&upstream_tip);
        let delta = f.delta().unwrap();
        assert_eq!(delta.count, 0, "the content is level, so nothing is new");
        assert_eq!(delta.basis, Basis::Recorded(upstream_tip));
    }

    /// A zero delta is what retires a stale pull request, so it has to be
    /// reachable — under ancestry-after-squash it never is.
    #[test]
    fn a_level_fork_reports_an_empty_delta() {
        let f = Fixture::new();
        let tip = f.upstream_commits(2);
        f.squash_merge_upstream();
        f.record_boundary(&tip);
        assert!(f.delta().unwrap().is_empty());
    }

    #[test]
    fn only_commits_after_the_boundary_are_counted() {
        let f = Fixture::new();
        let tip = f.upstream_commits(3);
        f.squash_merge_upstream();
        f.record_boundary(&tip);
        f.upstream_commits(2);

        let delta = f.delta().unwrap();
        assert_eq!(delta.count, 2, "two landed since the boundary");
        assert_eq!(
            f.git.count_commits("main", "up").unwrap(),
            5,
            "ancestry would have said five"
        );
    }

    #[test]
    fn the_boundary_is_read_from_the_base_branch_so_closing_a_pr_unmerged_changes_nothing() {
        let f = Fixture::new();
        let tip = f.upstream_commits(3);
        f.squash_merge_upstream();
        f.record_boundary(&tip);
        let before = f.delta().unwrap();

        // A sync run publishes a branch recording a newer boundary. Nobody
        // merges it, and the pull request is closed.
        let newer = f.upstream_commits(2);
        f.git.run(&["checkout", "--quiet", "-B", "upstream-sync", "up"]).unwrap();
        std::fs::write(f.git.root().join(BOUNDARY), format!("{newer}\n")).unwrap();
        f.git.run(&["add", BOUNDARY]).unwrap();
        f.git.run(&["commit", "--quiet", "-m", "Record boundary"]).unwrap();
        f.checkout("main");

        let after = f.delta().unwrap();
        assert_eq!(after.basis, before.basis, "the boundary did not move");
        assert_eq!(after.count, 2, "so the same work is still outstanding");
    }

    #[test]
    fn a_trailing_newline_is_tolerated() {
        let f = Fixture::new();
        let tip = f.upstream_commits(1);
        f.write_boundary_verbatim(&format!("  {tip}\n\n"));
        assert_eq!(
            read(&f.git, "main", BOUNDARY).unwrap(),
            Boundary::Recorded(tip)
        );
    }

    #[test]
    fn an_abbreviated_commit_id_is_accepted_because_the_forks_were_seeded_by_hand() {
        let f = Fixture::new();
        let tip = f.upstream_commits(2);
        f.write_boundary_verbatim(&format!("{}\n", &tip[..10]));
        let delta = f.delta().unwrap();
        assert_eq!(delta.count, 0);
    }

    #[test]
    fn an_empty_boundary_file_is_an_error_rather_than_a_silent_fallback() {
        let f = Fixture::new();
        f.upstream_commits(3);
        f.write_boundary_verbatim("\n");
        let err = f.delta().unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn a_corrupt_boundary_file_is_an_error_rather_than_a_silent_fallback() {
        let f = Fixture::new();
        f.upstream_commits(3);
        f.write_boundary_verbatim("not a sha at all\n");
        let err = f.delta().unwrap_err().to_string();
        assert!(err.contains("not a commit id"), "{err}");
        assert!(err.contains("ancestry"), "the error explains what was refused: {err}");
    }

    #[test]
    fn a_boundary_naming_a_commit_we_do_not_have_is_an_error() {
        let f = Fixture::new();
        f.upstream_commits(3);
        f.write_boundary_verbatim(&format!("{}\n", "0".repeat(40)));
        let err = f.delta().unwrap_err().to_string();
        assert!(err.contains("not in this repository"), "{err}");
    }

    #[test]
    fn an_absent_boundary_is_reported_as_absent_not_as_corrupt() {
        let f = Fixture::new();
        assert_eq!(read(&f.git, "main", BOUNDARY).unwrap(), Boundary::Absent);
    }

    #[test]
    fn a_commit_id_is_told_apart_from_prose() {
        assert!(is_commit_id("636e505c5cd809bdce37314f77130ffb4e45c46b"));
        assert!(is_commit_id("636e505"));
        assert!(!is_commit_id("636e50"), "too short to be unambiguous");
        assert!(!is_commit_id("main"));
        assert!(!is_commit_id("636e505c5cd809bdce37314f77130ffb4e45c46b\nextra"));
        assert!(!is_commit_id(""));
    }
}
