# weir

Keeps a fork on a self-hosted forge in step with its upstream, and puts the
result behind a pull request so a human sees it before it lands.

> **Status: early.** The config schema and the commit-counting logic are written
> and tested. The parts that actually touch a repository — merging, pushing,
> opening pull requests — are not built yet. It does not do anything useful today.

A weir is a low dam that does not block a river. It regulates what passes and
lets you measure it on the way over, which is what this does with upstream
commits.

## Why this exists

If you self-host Gitea or Forgejo and maintain a fork you have genuinely
modified, nothing quite fits:

| | |
|---|---|
| **Gitea "merge upstream" / Forgejo "sync fork"** | Writes straight to your branch, no review. Requires the parent to be a repo on the same instance, so a GitHub upstream is out. Errors on conflict. |
| **Pull mirrors** | Force-overwrite by design. They will erase your changes. |
| **`gh repo sync`** | Fast-forward only. Dies the moment you have your own commits, and GitHub-only. |
| **`wei/pull`** | Opens pull requests, which is right — but it is a GitHub App and cannot talk to a self-hosted forge. |
| **Copybara** | Built for rule-based transformation between repositories, not hand-written divergence, and has no self-hosted forge destination. |
| **Renovate / Dependabot** | Bump dependency versions inside a repo. No concept of an upstream project; cannot sync a fork at all. |

The gap is the combination: **a pull request gate, against a self-hosted
forge.** Everything forge-native skips review; the two tools that do review are
GitHub-only.

`weir` does not resolve conflicts. When a merge conflicts it says so and hands
you a branch to finish by hand. Judgement is not a feature anyone ships.

## How it works

For each fork it is told about, it clones your copy, fetches the real upstream,
and merges. On a clean merge, the sync branch is your base branch plus a merge
commit. On a conflict it publishes upstream's tip instead, so the forge marks
the pull request unmergeable and blocks the merge button rather than offering to
commit conflict markers. Either way it force-pushes the sync branch and opens or
refreshes a pull request.

The sync branch is force-pushed on every run. Anything committed there and not
merged is gone at the next sync.

## Configuration

No secrets live in the config file — only the names of environment variables
that carry them, so the file is safe to commit.

```toml
version = 1

[forge]
kind  = "gitea"          # or "forgejo" — same API
url   = "https://gitea.example.com"
owner = "my-org"
# token_env = "WEIR_TOKEN"   # a machine account, write:repository scope is enough

[[fork]]
repo     = "codex"
upstream = "https://github.com/openai/codex.git"
branch   = "main"
# Paths this fork removes on purpose that upstream keeps editing. Deleted on
# conflict rather than left conflicting, so an otherwise clean sync stays clean.
drop_on_conflict = [".github/workflows/rust-release.yml"]

[[fork]]
repo     = "dokploy"
upstream = "https://github.com/Dokploy/dokploy.git"
branch   = "canary"      # forks need not agree about their base branch
```

Public upstreams need no credential at all — they are cloned anonymously. Only
add one if you are rate-limited or syncing something private.

```console
$ weir validate --config forks.toml
```

## The boundary file

`weir` writes a file, `.upstream-sync`, recording which upstream commit the
fork's content corresponds to. It is committed onto the sync branch, so it lands
on your base branch when the pull request is merged.

This exists because **squash-merging destroys every implicit record of where a
fork sits.** After a squash, upstream's commits are not ancestors of your base
branch — only their content is. So asking git "which upstream commits are
unreachable from the fork" answers "all of them, since the fork began."

On the fork that motivated this, ancestry reported **663 new upstream commits
when roughly 44 had landed.** That inflates every pull request body, and it means
the path that retires a stale pull request can never run, because the count is
never zero. Tags and commit messages do not survive a squash either — a squash
rewrites the subject to name *your* pull request, not upstream's.

A file is content, so it survives a squash the way none of those do.

Two consequences worth knowing:

- It is read from the **base** branch, never the sync branch. Close a pull
  request without merging it and the boundary has not moved, so the next run
  recomputes the same delta and rebuilds the branch. That is deliberate.
- A **missing** boundary falls back to ancestry, which is correct for a fork
  that has never been synced. A boundary that is present but corrupt, empty, or
  names a commit not in the repository is an **error** — falling back there
  would silently reintroduce the inflated count.

If your forge can merge sync pull requests as merge commits rather than
squashes, prefer that and most of this becomes unnecessary.

## Licence

MIT.
