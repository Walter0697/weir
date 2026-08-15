# weir

Keeps a fork on a self-hosted forge in step with its upstream, and puts the
result behind a pull request so a human sees it before it lands.

> **Status: works, not yet packaged.** It clones, merges, builds and pushes the
> branch, and opens, refreshes, or retires the pull request. There is no
> container image and no scheduler yet — run it from a cron, a CI job, or by
> hand. Use `--dry-run` to point it at a live forge safely.

A weir is a low dam that does not block a river. It regulates what passes and
lets you measure it on the way over, which is what this does with upstream
commits.

## Quick start

```console
$ cat > forks.toml <<'EOF'
version = 1

[forge]
kind  = "gitea"
url   = "https://gitea.example.com"
owner = "my-org"

[[fork]]
repo     = "codex"
upstream = "https://github.com/openai/codex.git"
branch   = "main"
EOF

$ export WEIR_TOKEN=...            # a machine account, write:repository
$ weir run --dry-run               # look first; touches nothing
$ weir run                         # push the branch, open the pull request
```

Typical output:

```
codex: 75 new upstream commit(s) on main (counted from the recorded boundary 636e505c5cd8)
codex: CONFLICTS in 3 path(s); the branch is upstream's tip and the pull request will not be mergeable
codex:   codex-rs/tui/src/app.rs
codex: boundary 899d1715c87a504ce4c9ec85c2fd7753e33a7be4
codex: pushed upstream-sync at 8afc0373755f
codex: opened PR #16
```

## Why this exists

If you self-host Gitea or Forgejo and maintain a fork you have genuinely
modified, nothing quite fits:

| | |
|---|---|
| **Gitea "merge upstream" / Forgejo "sync fork"** | Merges straight into your branch, producing no pull request — nothing to read before it lands, and nothing for other automation to act on. Requires the parent to be a repo on the *same instance*, so a GitHub upstream cannot use it at all. On a conflict it errors and leaves you with nothing. |
| **Pull mirrors** | Force-overwrite by design. They will erase your changes. |
| **`gh repo sync`** | Fast-forward only. Dies the moment you have your own commits, and GitHub-only. |
| **`wei/pull`** | Opens pull requests, which is right — but it is a GitHub App and cannot talk to a self-hosted forge. |
| **Copybara** | Built for rule-based transformation between repositories, not hand-written divergence, and has no self-hosted forge destination. |
| **Renovate / Dependabot** | Bump dependency versions inside a repo. No concept of an upstream project; cannot sync a fork at all. |

The gap is the combination: **a pull request, against a self-hosted forge.**
Everything forge-native merges without producing one; the two tools that do open
pull requests are GitHub-only.

To be clear about what that buys, since it is easy to overstate: `weir` does not
review anything and does not resolve conflicts. It produces the thing a reviewer
works on — a branch and a pull request, with the outcome described — and then
stops. Whether a person reads it, a bot comments on it, or it sits there for a
week is not its business. Judgement is not a feature anyone ships.

| situation | what happens |
|---|---|
| Upstream and your fork edited the same file | Nothing automatic. The pull request is unmergeable and you resolve it. |
| Upstream edits a path you listed in `keep_removed` | Stays removed, and the removal is named in the pull request body. |
| Upstream adds a file your fork has never seen | Merges cleanly and arrives in the pull request. |

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
# Paths this fork deleted on purpose that upstream keeps editing. Enforced
# after every merge. Not a rule for resolving conflicts generally — anything
# without a rule here goes to a human.
keep_removed = [".github/workflows/rust-release.yml"]

[[fork]]
repo     = "dokploy"
upstream = "https://github.com/Dokploy/dokploy.git"
branch   = "canary"      # forks need not agree about their base branch
```

Public upstreams need no credential at all — they are cloned anonymously. Only
add one if you are rate-limited or syncing something private.

```console
$ weir validate --config forks.toml
$ weir run --config forks.toml --dry-run
$ weir run --config forks.toml --repo codex
```

`--dry-run` does everything except the two irreversible acts — the force-push
and the pull request — so it is safe against a live forge.

The token is read from the environment variable named in `token_env` and handed
to git through `GIT_ASKPASS`. It is never placed in a URL or a command line,
where the process list would expose it to every other user on the host.

## Running it

The container performs one pass and exits, so anything that can run a command on
a schedule can run it — cron, a systemd timer, a CI job, a Kubernetes CronJob.
There is deliberately no scheduler inside.

```console
$ export WEIR_TOKEN=...
$ docker run --rm \
    -v /etc/weir/forks.toml:/etc/weir/forks.toml:ro \
    -e WEIR_TOKEN \
    ghcr.io/walter0697/weir:latest \
    run --config /etc/weir/forks.toml
```

Pass the token as `-e WEIR_TOKEN` with **no value**, so docker forwards it by
name. Writing `-e WEIR_TOKEN="$(cat …)"` puts the secret in the docker client's
own argument list, where `ps` will show it to every user on the host.

Images are published to `ghcr.io/walter0697/weir`. Every commit on `main`
publishes `edge` and `sha-<commit>`; `latest` and the semantic version tags are
reserved for releases, so `edge` never becomes `latest` by accident.

The image runs as an unprivileged user (uid 10001), because it clones upstream
code and runs git over it. Your mounted config must be readable by that user —
it holds no secrets, so world-readable is fine, and a config mounted at mode
`600` from another uid will fail with a permission error.

The token is the only secret, and it is only ever an environment variable.

## What weir guarantees

If you build anything on top of a sync — a review job, a bot, a checklist —
these are the things it may rely on. They are the contract, and breaking one is
a breaking change.

1. **The sync branch has a fixed name.** `defaults.sync_branch`, `upstream-sync`
   unless you change it. It is force-pushed on every run.
2. **At most one open pull request per fork has that branch as its head ref.**
   That is how you find it; nothing else identifies it.
3. **After a merge, the boundary file on the base branch holds the upstream
   commit the fork's content corresponds to.** It moves only when a pull request
   merges.
4. **On a clean merge**, the sync branch is your base branch, plus a merge commit
   for upstream, plus the boundary commit.
5. **On a conflict**, the sync branch is *upstream's tip* plus the boundary
   commit. It deliberately contains none of your fork's own commits — that is
   what makes the forge refuse to merge it.

Point 5 has a consequence worth stating on its own, because getting it backwards
silently discards fork work:

> **To resolve a conflicting sync, merge the base branch *into* the sync branch,
> never the reverse.** On conflict the sync branch does not contain your fork's
> commits, so merging it into your base branch would present all of your own work
> as deletions.

Note also that the sync branch tip is **not** a pristine upstream commit even in
the conflict case — the boundary commit sits on top of it. Anything assuming the
tip is exactly upstream is wrong.

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
