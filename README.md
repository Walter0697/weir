# weir

Keeps a fork on a self-hosted forge in step with its upstream, and puts the
result in a pull request instead of merging it.

> **Status: usable, early.** It clones, merges, builds and pushes the branch, and
> opens, refreshes, or retires the pull request. Run it once from a cron or CI
> job, or run `weir serve` for a web UI with its own schedule. Use `--dry-run`
> to point it at a live forge safely.

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

`weir`'s job ends at the pull request. It builds the branch, describes what
happened, and stops. It does not merge, and it does not resolve conflicts —
when a merge conflicts it says which paths and leaves the branch for whoever
picks it up.

| situation | what happens |
|---|---|
| Upstream and your fork edited the same file | Nothing automatic. The pull request is unmergeable and you resolve it. |
| Upstream edits a path you listed in `keep_removed` | Stays removed — and the pull request names the path *and every upstream commit that touched it*, so you can see what was discarded. |
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
# without a rule here is left alone.
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

### Environment

Secrets are the only thing that comes from the environment. Everything else is
in `forks.toml`.

| variable | needed | what it is |
|---|---|---|
| `WEIR_TOKEN` | **yes** | Forge machine account token, `write:repository` scope. Rename it with `forge.token_env`. |
| `TELEGRAM_BOT_TOKEN` | only with `[[notify]]` | Bot token from BotFather. Rename with `token_env`. |
| `TELEGRAM_CHAT_ID` | only with `[[notify]]` | Chat or channel to post to. Rename with `chat_env`. |

Nothing is needed for GitHub. Public upstreams are cloned anonymously; add a
credential only for a private upstream or if you hit rate limits.

`weir validate` reports which of these are actually set, so a channel that would
silently stay quiet is visible before you depend on it.

### With docker compose

`weir` exits when it is done, so it is not a service that stays up — use
`docker compose run`. A full example is in
[`compose.example.yaml`](compose.example.yaml).

```yaml
services:
  weir:
    image: ghcr.io/walter0697/weir:edge
    env_file: [weir.env]
    volumes:
      - ./forks.toml:/etc/weir/forks.toml:ro
    command: ["run", "--config", "/etc/weir/forks.toml"]
```

```console
$ cat > weir.env <<'EOF'
WEIR_TOKEN=...
TELEGRAM_BOT_TOKEN=...
TELEGRAM_CHAT_ID=...
EOF
$ chmod 600 weir.env

$ docker compose run --rm weir run --dry-run     # look first
$ docker compose run --rm weir run               # do it
```

An `env_file` keeps the trailing newline that shell substitution strips, so
`weir` trims the values it reads — a token with a stray newline fails to
authenticate and says nothing useful about why.

To put it on a schedule, point cron or a systemd timer at the same command:

```cron
0 5 * * 5  cd /srv/weir && docker compose run --rm weir run >> /var/log/weir.log 2>&1
```

## Notifications

An unattended sync is invisible unless it says something, so `weir` can send one
message per fork per run — including when a sync **fails**, which is the outcome
most worth hearing about.

```toml
[[notify]]
kind = "telegram"
# token_env = "TELEGRAM_BOT_TOKEN"
# chat_env  = "TELEGRAM_CHAT_ID"
```

Like the forge token, only the environment variable *names* live in the config.
`weir validate` reports whether they are actually set, so a channel that would
silently stay quiet is visible before you rely on it:

```console
$ weir validate
  notifications: telegram (reads TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID) — both set
```

Sending is **best effort and always last**. The branch is pushed and the pull
request reconciled before anything is sent, so a missing token or an outage at
the other end can never turn a completed sync into a failed one — it prints a
note and carries on.

## The web UI

`weir serve` is the alternative to editing a file: it keeps its configuration in
SQLite, draws a UI for it, and owns a schedule.

```console
$ weir serve --db /data/weir.db --bind 127.0.0.1:8080
weir: listening on http://127.0.0.1:8080
```

It offers the repositories already on your forge rather than making you type
them, and fills in the upstream from what each was migrated from — Gitea records
that as `original_url`, which is usually exactly right. Check it before saving.

From there: edit a fork, set a cron schedule, press **Dry run** to see what a
sync would do, or **Sync all** to do it. Every run is kept with its full output,
so you can read last Friday's without having been watching.

**Stop** appears while something is running. Cancellation is cooperative,
because it has to be — the work is child `git` processes and blocking HTTP, and
none of that can be interrupted from outside. What it does is kill the `git`
process running right now and stop before the next repository, which in practice
means a stop lands within a second or two even during a long clone.

Stopping is always safe. Every run rebuilds its branch from scratch, so a sync
that was interrupted after pushing leaves a branch the next run simply replaces,
and one interrupted before pushing leaves nothing at all.

### Watching an owner

Listing repositories one at a time stops being reasonable somewhere around the
fifth. A **watch** covers everything under one owner and is worked out fresh on
every run, so a repository added to the forge next week is included without
anyone editing anything.

Three things narrow it, and the page shows all three rather than applying them
quietly — a rule whose effect you cannot see is a rule you cannot trust:

```
walter-opensource — covers 1 repo(s), skips 3

  syncs now:     dokploy (canary)
  leaves alone:  codex          — configured as its own fork
                 renovate-bot   — no upstream recorded on the forge
                 renovate-config — no upstream recorded on the forge
```

- **Exceptions** you write. Names, or `*` patterns like `test-*`. A bare `*`
  excepts everything, which pauses a watch without losing what you wrote.
- **Forks configured by hand win.** That is how one repository keeps its own
  `keep_removed` or a different upstream branch while the rest are covered by
  the rule.
- **No recorded upstream, no sync.** There is nothing to sync *from*, so it is
  skipped and said out loud.

**Loopback by default, on purpose.** Anything that can reach this can change
which repositories get force-pushed.

To reach it from elsewhere, set `WEIR_UI_TOKEN` and bind wider:

```console
$ WEIR_UI_TOKEN=$(head -c 18 /dev/urandom | base64) weir serve --bind 0.0.0.0:8080
weir: listening on http://0.0.0.0:8080
weir: an access token is required (WEIR_UI_TOKEN)
```

One token, entered once, kept in an `HttpOnly` cookie. There are no accounts
because there is nothing to distinguish between — everyone who gets in can do
everything. Comparison is constant-time, so a near miss costs the same as a
wild guess.

Without the variable it stays open, and binding to anything other than loopback
then prints a warning at startup naming exactly what is exposed. Behind a
reverse proxy that already authenticates, leaving it open is reasonable; on a
LAN it is not.

Note the cookie is not marked `Secure`, because this is usually served over
plain HTTP on a home network and a `Secure` cookie would never be stored. That
also means the token crosses that network in the clear — put it behind TLS if
that matters to you.

**The database holds your forge token**, so treat the volume as a secret. It is
created mode 0600, never rendered back to the browser, and never written to the
audit trail — the trail records only that the token changed.

**Two front ends, never merged.** `weir run --config` reads a TOML file and does
not open the database; `weir serve` reads the database and ignores `--config`.
There is always exactly one answer to where a setting came from.

What is *not* in the database is the sync boundary. That stays a file in each
repository, so closing a pull request unmerged still costs nothing, and losing
the database costs you settings and history — not correctness.

## What weir guarantees

If you build anything on top of a sync, these are the things it may rely on.
They are the contract, and breaking one is a breaking change.

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

## What `keep_removed` costs you

Holding a path out of the fork discards **everything** upstream does inside it.
That is usually what you want — the file was deleted to be rid of something. But
a file is a crude unit, and upstream may later put something worth having in the
same one. Nothing can decide that for you.

So the sync does not try. It reports what it threw away:

```
codex: kept removed: .github/workflows/python-runtime-build.yml (unchanged upstream since the last sync)
codex: kept removed: .github/workflows/rust-release.yml (1 upstream commit discarded with it)
```

and the pull request body lists the commit subjects, capped at five with a count
for the rest. If one of them is something you want, take the path out of
`keep_removed` for a run and resolve the conflict by hand, or lift the change
into wherever your fork keeps that behaviour now.

The same report tells you when an entry has gone stale: a path upstream no
longer carries stops appearing at all.

## Licence

MIT.
