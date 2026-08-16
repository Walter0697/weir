//! Building the sync branch.
//!
//! Everything here happens in a clone and is idempotent: the branch is rebuilt
//! from scratch on every run and force-pushed, so a run that dies half way
//! leaves nothing to clean up.
//!
//! The branch is built *from the fork*, then upstream is merged into it. Doing
//! it the other way — starting from upstream and merging the fork in — makes
//! the forge diff the pull request against a base it does not share, and every
//! fork-owned file shows up as a deletion.

use crate::boundary::{self, Delta};
use crate::git::Git;
use anyhow::{Context, Result};

/// What to build, resolved from config for one fork.
pub struct Plan {
    pub base_branch: String,
    pub upstream_branch: String,
    pub sync_branch: String,
    pub boundary_file: String,
    pub keep_removed: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Merge {
    /// Upstream merged into the fork without help.
    Clean,
    /// It did not, so the branch is upstream's tip instead and these paths are
    /// what a human has to reconcile.
    Conflicted { paths: Vec<String> },
}

impl Merge {
    pub fn conflicted(&self) -> bool {
        matches!(self, Merge::Conflicted { .. })
    }
}

/// A path held out of the fork, and what upstream did to it meanwhile.
#[derive(Debug, PartialEq, Eq)]
pub struct Removed {
    pub path: String,
    /// Upstream commits that touched this path since the boundary — precisely
    /// what keeping it removed is throwing away.
    ///
    /// This exists because `keep_removed` is lossy in a way that is easy to
    /// forget. A file is usually dropped to be rid of one feature, and upstream
    /// may later put something worth having *inside the same file*. Nothing can
    /// decide that automatically, so the sync reports what it discarded and
    /// leaves the reading to whoever wants it.
    pub upstream_commits: Vec<String>,
}

pub struct Built {
    pub delta: Delta,
    pub merge: Merge,
    /// Paths removed to honour `keep_removed`, with what was discarded along
    /// with them, so nothing goes silently.
    pub removed: Vec<Removed>,
    /// The upstream commit this sync carries.
    pub upstream_sha: String,
    /// The tip of the sync branch once built.
    pub tip: String,
}

pub enum Sync {
    /// Nothing new upstream. Whatever pull request is open is stale.
    UpToDate {
        delta: Delta,
    },
    Built(Built),
}

/// Fetches upstream and builds the sync branch in an existing clone of the fork.
///
/// The clone's `origin` must be the fork; `upstream` is added here.
pub fn build(git: &Git, plan: &Plan, upstream_url: &str) -> Result<Sync> {
    git.add_remote("upstream", upstream_url)
        .context("adding the upstream remote")?;
    git.fetch("upstream", &plan.upstream_branch)
        .with_context(|| format!("fetching {upstream_url} at {}", plan.upstream_branch))?;

    let base_ref = format!("origin/{}", plan.base_branch);
    let upstream_ref = format!("upstream/{}", plan.upstream_branch);

    let delta = boundary::delta(git, &base_ref, &upstream_ref, &plan.boundary_file)?;
    if delta.is_empty() {
        return Ok(Sync::UpToDate { delta });
    }

    let upstream_sha = git.rev_parse(&upstream_ref)?;

    // Start from the fork so its own commits and files stay in the pull request.
    git.checkout_new(&plan.sync_branch, &base_ref)?;

    let merge = if git.merge(&upstream_ref)? {
        Merge::Clean
    } else {
        resolve_or_give_up(git, plan, &upstream_ref)?
    };

    // Enforced on both paths, and after the merge rather than only during it:
    // upstream may re-add one of these in a later commit, which conflicts with
    // nothing and would otherwise let the file quietly return.
    enforce_keep_removed(git, &plan.keep_removed)?;

    // Reported by comparing the result against upstream rather than by
    // recording what each step did. A path can be taken out while resolving the
    // merge or afterwards, and the pull request should say the same thing
    // either way: upstream has this, we deliberately do not.
    // Counted from the boundary when there is one, so the report covers exactly
    // the same span as the commit count and does not repeat what an earlier
    // sync already showed.
    let since = match &delta.basis {
        boundary::Basis::Recorded(sha) => sha.clone(),
        boundary::Basis::Ancestry => base_ref.clone(),
    };
    let removed = removed_against(git, &plan.keep_removed, &upstream_ref, &since)?;

    record_boundary(git, &plan.boundary_file, &upstream_sha)?;

    Ok(Sync::Built(Built {
        delta,
        merge,
        removed,
        upstream_sha,
        tip: git.rev_parse("HEAD")?,
    }))
}

/// A conflicted merge gets exactly one chance: drop the paths this fork keeps
/// removed, which is the one conflict whose answer never changes. Anything
/// still unresolved is a human's decision.
fn resolve_or_give_up(git: &Git, plan: &Plan, upstream_ref: &str) -> Result<Merge> {
    for path in &plan.keep_removed {
        git.remove(path)
            .with_context(|| format!("keeping {path} removed during the merge"))?;
    }

    let conflicts = git.conflicted_paths()?;
    if conflicts.is_empty() {
        git.run(&["commit", "--quiet", "--no-edit"])
            .context("concluding the merge")?;
        return Ok(Merge::Clean);
    }

    // Publish upstream's tip rather than a half-merged tree. The forge then
    // marks the pull request unmergeable and blocks the merge button, instead
    // of offering to commit conflict markers into the base branch.
    //
    // The branch deliberately carries none of the fork's commits, which is why
    // resolving one means merging the base branch *into* it, never the reverse.
    git.merge_abort()?;
    git.checkout_new(&plan.sync_branch, upstream_ref)?;
    Ok(Merge::Conflicted { paths: conflicts })
}

/// Which of the listed paths upstream carries and this branch does not, and
/// what upstream changed in them since `since`.
fn removed_against(
    git: &Git,
    paths: &[String],
    upstream_ref: &str,
    since: &str,
) -> Result<Vec<Removed>> {
    let mut removed = Vec::new();
    for path in paths {
        if git.path_exists_at(upstream_ref, path)? && !git.is_tracked(path)? {
            removed.push(Removed {
                path: path.clone(),
                upstream_commits: git.commits_touching(since, upstream_ref, path)?,
            });
        }
    }
    Ok(removed)
}

fn enforce_keep_removed(git: &Git, paths: &[String]) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for path in paths {
        if git.is_tracked(path)? {
            git.remove(path)?;
            removed.push(path.clone());
        }
    }
    if !removed.is_empty() && git.has_staged_changes()? {
        let message = format!(
            "Keep {} path(s) removed that this fork does not carry\n\n{}",
            removed.len(),
            removed
                .iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        git.commit(&message)?;
    }
    Ok(removed)
}

/// Writes the boundary as a file in the tree.
///
/// It is committed onto the sync branch so it reaches the base branch when the
/// pull request merges — including through a squash, which is the whole reason
/// this is content rather than a tag or a commit message.
fn record_boundary(git: &Git, boundary_file: &str, upstream_sha: &str) -> Result<()> {
    let path = git.root().join(boundary_file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {} for the boundary file", parent.display()))?;
    }
    std::fs::write(&path, format!("{upstream_sha}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    git.add(boundary_file)?;
    if git.has_staged_changes()? {
        git.commit(&format!("Record upstream sync boundary {upstream_sha}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::Basis;
    use crate::git::Cancel;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    const BOUNDARY: &str = ".upstream-sync";
    const DROPPED: &str = "ci/upstream-only.yml";

    /// An upstream repository, a bare fork that has diverged from it, and a
    /// working clone of the fork with `origin` pointing at it — the same shape
    /// a real run works in.
    struct Sandbox {
        _dir: TempDir,
        upstream: PathBuf,
        fork: PathBuf,
        work: Git,
    }

    fn git_at(path: &Path) -> Git {
        Git::new(path)
    }

    fn commit(git: &Git, name: &str, contents: &str) -> String {
        let path = git.root().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        git.run(&["add", "-A"]).unwrap();
        git.run(&["commit", "--quiet", "-m", name]).unwrap();
        git.rev_parse("HEAD").unwrap()
    }

    impl Sandbox {
        /// `upstream` starts with a shared base plus a file the fork will
        /// delete; the fork then removes it and adds a file of its own.
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let upstream = dir.path().join("upstream");
            let fork = dir.path().join("fork.git");
            let seed = dir.path().join("seed");
            let work = dir.path().join("work");

            std::fs::create_dir_all(&upstream).unwrap();
            let up = git_at(&upstream);
            up.run(&["init", "--quiet", "--initial-branch=main"])
                .unwrap();
            commit(&up, "shared.txt", "shared\n");
            commit(&up, DROPPED, "upstream ci\n");

            Git::new(dir.path())
                .run(&[
                    "init",
                    "--quiet",
                    "--bare",
                    "--initial-branch=main",
                    "fork.git",
                ])
                .unwrap();

            let seeded = Git::clone_repo(
                upstream.to_str().unwrap(),
                "main",
                &seed,
                None,
                Cancel::new(),
            )
            .unwrap();
            seeded.run(&["rm", "--quiet", "--", DROPPED]).unwrap();
            seeded
                .run(&[
                    "commit",
                    "--quiet",
                    "-m",
                    "Drop upstream CI; this fork uses its own",
                ])
                .unwrap();
            commit(&seeded, "fork-only.txt", "ours\n");
            seeded.add_remote("fork", fork.to_str().unwrap()).unwrap();
            seeded.run(&["push", "--quiet", "fork", "main"]).unwrap();

            let work = Git::clone_repo(fork.to_str().unwrap(), "main", &work, None, Cancel::new())
                .unwrap();

            Self {
                _dir: dir,
                upstream,
                fork,
                work,
            }
        }

        fn upstream(&self) -> Git {
            git_at(&self.upstream)
        }

        fn plan(&self, keep_removed: &[&str]) -> Plan {
            Plan {
                base_branch: "main".to_string(),
                upstream_branch: "main".to_string(),
                sync_branch: "upstream-sync".to_string(),
                boundary_file: BOUNDARY.to_string(),
                keep_removed: keep_removed.iter().map(|s| s.to_string()).collect(),
            }
        }

        fn build(&self, keep_removed: &[&str]) -> Result<Sync> {
            build(
                &self.work,
                &self.plan(keep_removed),
                self.upstream.to_str().unwrap(),
            )
        }

        fn built(&self, keep_removed: &[&str]) -> Built {
            match self.build(keep_removed).expect("build should succeed") {
                Sync::Built(built) => built,
                Sync::UpToDate { .. } => panic!("expected work to do"),
            }
        }

        fn tracked(&self, path: &str) -> bool {
            self.work.is_tracked(path).unwrap()
        }
    }

    #[test]
    fn a_clean_merge_keeps_the_forks_own_files_and_takes_upstreams() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");

        let built = sandbox.built(&[]);

        assert_eq!(built.merge, Merge::Clean);
        assert_eq!(built.delta.count, 1);
        assert!(sandbox.tracked("fork-only.txt"), "the fork's file survives");
        assert!(
            sandbox.tracked("new-from-upstream.txt"),
            "upstream's arrives"
        );
    }

    #[test]
    fn the_boundary_is_recorded_on_the_sync_branch() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");

        let built = sandbox.built(&[]);

        let recorded = sandbox
            .work
            .show_file("HEAD", BOUNDARY)
            .unwrap()
            .expect("the boundary file should exist");
        assert_eq!(recorded.trim(), built.upstream_sha);
    }

    #[test]
    fn nothing_new_upstream_is_reported_rather_than_built() {
        let sandbox = Sandbox::new();
        match sandbox.build(&[]).unwrap() {
            Sync::UpToDate { delta } => assert_eq!(delta.count, 0),
            Sync::Built(_) => panic!("there was nothing to sync"),
        }
    }

    #[test]
    fn the_first_sync_of_a_never_synced_fork_counts_from_ancestry() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");
        let built = sandbox.built(&[]);
        assert_eq!(built.delta.basis, Basis::Ancestry);
    }

    /// Both sides edited the same file. Nothing is guessed at.
    #[test]
    fn a_real_conflict_publishes_upstreams_tip_and_names_the_paths() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "shared.txt", "upstream's version\n");
        // The fork edits the same file.
        sandbox.work.run(&["checkout", "--quiet", "main"]).unwrap();
        commit(&sandbox.work, "shared.txt", "our version\n");
        sandbox
            .work
            .run(&["push", "--quiet", "origin", "main"])
            .unwrap();
        sandbox.work.run(&["fetch", "--quiet", "origin"]).unwrap();

        let built = sandbox.built(&[]);

        assert_eq!(
            built.merge,
            Merge::Conflicted {
                paths: vec!["shared.txt".to_string()]
            }
        );
        assert!(
            !sandbox.tracked("fork-only.txt"),
            "the branch is upstream's tip, so it carries none of the fork's commits"
        );
    }

    #[test]
    fn a_conflicting_sync_still_records_the_boundary_on_top_of_upstreams_tip() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "shared.txt", "upstream's version\n");
        sandbox.work.run(&["checkout", "--quiet", "main"]).unwrap();
        commit(&sandbox.work, "shared.txt", "our version\n");
        sandbox
            .work
            .run(&["push", "--quiet", "origin", "main"])
            .unwrap();
        sandbox.work.run(&["fetch", "--quiet", "origin"]).unwrap();

        let built = sandbox.built(&[]);

        assert!(built.merge.conflicted());
        assert_ne!(
            built.tip, built.upstream_sha,
            "the tip is upstream plus the boundary commit, not bare upstream"
        );
        let recorded = sandbox.work.show_file("HEAD", BOUNDARY).unwrap().unwrap();
        assert_eq!(recorded.trim(), built.upstream_sha);
    }

    /// The delete/modify conflict this fork gets every time upstream touches a
    /// file the fork removed. The answer never changes, so it is declared once.
    #[test]
    fn a_path_kept_removed_resolves_its_conflict_instead_of_blocking_the_sync() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), DROPPED, "upstream edited their ci\n");

        let built = sandbox.built(&[DROPPED]);

        assert_eq!(built.merge, Merge::Clean, "the only conflict had a rule");
        assert!(!sandbox.tracked(DROPPED), "and it stayed removed");
        assert!(sandbox.tracked("fork-only.txt"), "the fork's work survived");
    }

    #[test]
    fn without_the_rule_that_same_edit_is_a_conflict_left_for_a_human() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), DROPPED, "upstream edited their ci\n");

        let built = sandbox.built(&[]);

        assert!(
            built.merge.conflicted(),
            "nothing is dropped without being asked for"
        );
    }

    /// Upstream re-adding the path in a later commit conflicts with nothing, so
    /// a conflict-only rule would let it back in.
    #[test]
    fn a_path_kept_removed_is_taken_out_again_when_upstream_re_adds_it() {
        let sandbox = Sandbox::new();
        let up = sandbox.upstream();
        up.run(&["rm", "--quiet", "--", DROPPED]).unwrap();
        up.run(&["commit", "--quiet", "-m", "upstream drops it too"])
            .unwrap();
        commit(&up, DROPPED, "upstream brings it back\n");

        let built = sandbox.built(&[DROPPED]);

        assert_eq!(
            built.merge,
            Merge::Clean,
            "re-adding conflicts with nothing"
        );
        assert_eq!(
            built
                .removed
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec![DROPPED]
        );
        assert!(
            !built.removed[0].upstream_commits.is_empty(),
            "the commit that re-added it is reported as discarded"
        );
        assert!(!sandbox.tracked(DROPPED));
    }

    /// The honest limit of `keep_removed`: dropping a path throws away every
    /// upstream change inside it, including ones worth having. Nothing can judge
    /// that automatically, so the sync has to say what it discarded.
    #[test]
    fn keeping_a_path_removed_reports_the_upstream_commits_it_discards() {
        let sandbox = Sandbox::new();
        let up = sandbox.upstream();
        commit(&up, DROPPED, "upstream adds something here\n");
        commit(&up, DROPPED, "upstream adds a second thing here\n");
        commit(&up, "elsewhere.txt", "unrelated\n");

        let built = sandbox.built(&[DROPPED]);

        let dropped = built
            .removed
            .iter()
            .find(|r| r.path == DROPPED)
            .expect("the path was kept removed");
        assert_eq!(
            dropped.upstream_commits.len(),
            2,
            "both commits touching it are named, and the unrelated one is not: {:?}",
            dropped.upstream_commits
        );
    }

    #[test]
    fn a_removal_is_reported_so_the_pull_request_can_say_what_went() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");
        let built = sandbox.built(&[]);
        assert!(
            built.removed.is_empty(),
            "nothing was asked for, nothing went"
        );
    }

    #[test]
    fn the_sync_branch_is_force_pushed_to_the_fork() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");
        let built = sandbox.built(&[]);

        sandbox.work.force_push("origin", "upstream-sync").unwrap();

        let on_fork = git_at(&sandbox.fork)
            .rev_parse("refs/heads/upstream-sync")
            .unwrap();
        assert_eq!(on_fork, built.tip);
    }

    #[test]
    fn building_twice_produces_the_same_branch() {
        let sandbox = Sandbox::new();
        commit(&sandbox.upstream(), "new-from-upstream.txt", "theirs\n");

        let first = sandbox.built(&[]);
        let first_tree = sandbox.work.rev_parse("HEAD^{tree}").unwrap();
        sandbox.work.run(&["checkout", "--quiet", "main"]).unwrap();
        sandbox.work.run(&["remote", "remove", "upstream"]).unwrap();
        let second = sandbox.built(&[]);
        let second_tree = sandbox.work.rev_parse("HEAD^{tree}").unwrap();

        // Trees, not commit ids: a commit id embeds the time it was made, so
        // two identical runs a second apart hash differently.
        assert_eq!(first_tree, second_tree, "the run is idempotent");
        assert_eq!(first.upstream_sha, second.upstream_sha);
    }
}
