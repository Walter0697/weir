//! The pages themselves.
//!
//! Server-rendered HTML with no build step and no JavaScript, because this is a
//! settings form and a table. Everything is inline, so the deliverable stays
//! one binary in one container with nothing to fetch at runtime — a
//! self-hosted tool that phones out to a CDN to draw its own settings page is
//! a bad trade.

use super::{discover, App};
use crate::store::{Fork, Run};
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use maud::{html, Markup, DOCTYPE};

const STYLE: &str = r#"
:root { color-scheme: light dark; --bg:#fbfbfa; --fg:#1a1a1a; --muted:#666; --line:#e3e3e0;
        --card:#fff; --accent:#2f6f4f; --warn:#8a5a00; --bad:#a33; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#16181a; --fg:#e8e8e6; --muted:#9a9a97; --line:#2c2f33; --card:#1d2023;
          --accent:#6fbf8f; --warn:#d0a04a; --bad:#e07070; }
}
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.55 ui-sans-serif,system-ui,-apple-system,Segoe UI,Roboto,sans-serif; }
main { max-width: 62rem; margin: 0 auto; padding: 1.5rem 1.25rem 4rem; }
header { border-bottom:1px solid var(--line); }
header .bar { max-width:62rem; margin:0 auto; padding:.9rem 1.25rem; display:flex; gap:1.25rem; align-items:baseline; }
header a { color:var(--fg); text-decoration:none; opacity:.7; }
header a:hover, header a.on { opacity:1; }
h1 { font-size:1.05rem; margin:0 1rem 0 0; letter-spacing:.02em; }
h2 { font-size:.95rem; text-transform:uppercase; letter-spacing:.08em; color:var(--muted); margin:2rem 0 .75rem; }
.card { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:1rem 1.1rem; margin-bottom:1rem; }
table { width:100%; border-collapse:collapse; }
th,td { text-align:left; padding:.55rem .6rem; border-bottom:1px solid var(--line); vertical-align:top; }
th { font-size:.78rem; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); font-weight:600; }
tr:last-child td { border-bottom:none; }
label { display:block; margin:.85rem 0 .3rem; font-size:.85rem; color:var(--muted); }
input[type=text], input[type=password], textarea, select {
  width:100%; padding:.5rem .6rem; border:1px solid var(--line); border-radius:7px;
  background:var(--bg); color:var(--fg); font:inherit; font-size:.92rem; }
textarea { min-height:5rem; font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:.85rem; }
button, .btn { font:inherit; font-size:.88rem; padding:.45rem .9rem; border-radius:7px;
  border:1px solid var(--line); background:var(--card); color:var(--fg); cursor:pointer; text-decoration:none; display:inline-block; }
button.primary { background:var(--accent); border-color:var(--accent); color:#fff; }
button.danger { color:var(--bad); }
.row { display:flex; gap:.5rem; flex-wrap:wrap; align-items:center; }
.muted { color:var(--muted); }
.small { font-size:.85rem; }
code, .mono { font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:.85rem; }
pre { background:var(--bg); border:1px solid var(--line); border-radius:8px; padding:.8rem;
      overflow-x:auto; font-size:.82rem; line-height:1.5; }
.pill { font-size:.75rem; padding:.15rem .5rem; border-radius:999px; border:1px solid var(--line); }
.ok { color:var(--accent); } .warn { color:var(--warn); } .bad { color:var(--bad); }
.note { border-left:3px solid var(--warn); padding:.5rem .8rem; margin:.75rem 0; background:var(--card); }
.grid2 { display:grid; grid-template-columns:1fr 1fr; gap:0 1rem; }
@media (max-width:640px){ .grid2 { grid-template-columns:1fr; } }

/* Cross-document transitions, so moving between pages does not flash white.
   Browsers without it simply navigate as they always did. */
@view-transition { navigation: auto; }

/* Everything below is decoration and must be able to switch off entirely.
   Motion is a preference, and for some people it is a medical one. */
@media (prefers-reduced-motion: no-preference) {
  ::view-transition-old(root) { animation: fade-out .12s ease both; }
  ::view-transition-new(root) { animation: rise .18s cubic-bezier(.2,.7,.3,1) both; }

  main > * { animation: rise .22s cubic-bezier(.2,.7,.3,1) both; }
  /* A short stagger down the page, capped: past the fourth block the delay
     stops being pleasant and starts being a wait. */
  main > :nth-child(2) { animation-delay:.03s }
  main > :nth-child(3) { animation-delay:.06s }
  main > :nth-child(n+4) { animation-delay:.09s }

  tbody tr { animation: rise .2s ease both; }
  tbody tr:nth-child(2){animation-delay:.02s} tbody tr:nth-child(3){animation-delay:.04s}
  tbody tr:nth-child(4){animation-delay:.06s} tbody tr:nth-child(n+5){animation-delay:.08s}

  .card, tbody tr, button, .btn, a, summary, input, textarea { transition:
      background-color .16s ease, border-color .16s ease, color .16s ease,
      box-shadow .16s ease, transform .12s ease, opacity .16s ease; }
  details[open] > .details-body { animation: unfold .2s cubic-bezier(.2,.7,.3,1) both; }
  .dot { animation: pulse 1.4s ease-in-out infinite; }
}

@keyframes rise { from { opacity:0; transform:translateY(5px) } to { opacity:1; transform:none } }
@keyframes fade-out { to { opacity:0 } }
@keyframes unfold { from { opacity:0; transform:translateY(-4px) } to { opacity:1; transform:none } }
@keyframes pulse { 0%,100% { opacity:.35 } 50% { opacity:1 } }

tbody tr:hover { background:color-mix(in srgb, var(--accent) 6%, transparent); }
.card:hover { border-color:color-mix(in srgb, var(--accent) 28%, var(--line)); }
button:hover, .btn:hover { border-color:var(--accent); }
button:active, .btn:active { transform:translateY(1px); }
button.primary:hover { filter:brightness(1.08); }
button.danger:hover { border-color:var(--bad); }
a { color:var(--accent); }
:focus-visible { outline:2px solid var(--accent); outline-offset:2px; border-radius:5px; }
input:focus, textarea:focus { border-color:var(--accent);
  box-shadow:0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); outline:none; }

/* Collapsible sections. `details` does the work; this only makes it look
   deliberate rather than like a browser default. */
details { border:1px solid var(--line); border-radius:10px; background:var(--card); margin-bottom:1rem; }
details > summary { cursor:pointer; padding:.75rem 1.1rem; list-style:none; font-size:.9rem;
  display:flex; align-items:center; gap:.5rem; user-select:none; }
details > summary::-webkit-details-marker { display:none; }
details > summary::before { content:"›"; display:inline-block; font-size:1.1rem; line-height:1;
  color:var(--muted); transition:transform .18s cubic-bezier(.2,.7,.3,1); }
details[open] > summary::before { transform:rotate(90deg); }
details > summary:hover { color:var(--accent); }
.details-body { padding:0 1.1rem 1rem; }

/* A run still going, so the page says something is happening without polling. */
.dot { display:inline-block; width:.5rem; height:.5rem; border-radius:50%;
  background:currentColor; margin-right:.4rem; vertical-align:middle; }
"#;

fn shell(title: &str, current: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "weir — " (title) }
                style { (maud::PreEscaped(STYLE)) }
            }
            body {
                header { div class="bar" {
                    h1 { "weir" }
                    a href="/" class=(if current == "home" { "on" } else { "" }) { "Forks" }
                    a href="/settings" class=(if current == "settings" { "on" } else { "" }) { "Settings" }
                }}
                main { (body) }
            }
        }
    }
}

pub fn error_page(title: &str, detail: &str) -> Response {
    Html(
        shell(
            "problem",
            "",
            html! {
                h2 { "Something went wrong" }
                div class="card" {
                    p { strong { (title) } }
                    pre { (detail) }
                    a class="btn" href="/" { "Back" }
                }
            },
        )
        .into_string(),
    )
    .into_response()
}

fn outcome_class(outcome: Option<&str>) -> &'static str {
    match outcome {
        Some("clean") | Some("up to date") => "ok",
        Some("conflicts") => "warn",
        Some("failed") => "bad",
        _ => "muted",
    }
}

pub async fn dashboard(State(app): State<App>) -> impl IntoResponse {
    let forks = app.store().forks().unwrap_or_default();
    let runs = app.store().recent_runs(15).unwrap_or_default();
    let settings = app.store().settings().unwrap_or_default();
    let secrets = app.store().secret_status().ok();
    let configured = !settings.forge_url.is_empty() && !settings.forge_owner.is_empty();

    Html(
        shell(
            "forks",
            "home",
            html! {
                @if !configured || secrets.is_none_or(|s| !s.forge_token) {
                    div class="note" {
                        "Set the forge URL, owner, and token in "
                        a href="/settings" { "Settings" }
                        " before adding a fork — the forge cannot be reached or listed without them."
                    }
                }

                div class="row" style="justify-content:space-between; margin-bottom:.5rem" {
                    h2 style="margin:0" { "Forks" }
                    div class="row" {
                        a class="btn" href="/forks/new" { "Add fork" }
                        form method="post" action="/run" style="display:inline" {
                            input type="hidden" name="dry_run" value="1";
                            button type="submit" { "Dry run all" }
                        }
                        form method="post" action="/run" style="display:inline" {
                            button class="primary" type="submit" { "Sync all" }
                        }
                    }
                }

                @if forks.is_empty() {
                    div class="card muted" { "No forks configured yet." }
                } @else {
                    div class="card" { table {
                        thead { tr {
                            th { "Repository" } th { "Upstream" } th { "Branch" }
                            th { "Kept removed" } th {}
                        }}
                        tbody { @for fork in &forks { (fork_row(fork)) } }
                    }}
                }

                h2 { "Schedule" }
                div class="card small" {
                    @match &settings.schedule {
                        Some(cron) => {
                            "Scheduled " code { (cron) } " — server local time. "
                            a href="/settings" { "Change" }
                        }
                        None => {
                            span class="muted" { "Nothing is scheduled; runs happen only when you press a button. " }
                            a href="/settings" { "Set a schedule" }
                        }
                    }
                }

                h2 { "Recent runs" }
                @if runs.is_empty() {
                    div class="card muted small" { "Nothing has run yet." }
                } @else {
                    div class="card" { table {
                        thead { tr {
                            th { "Started" } th { "Repository" } th { "Result" } th {}
                        }}
                        tbody { @for run in &runs { (run_row(run)) } }
                    }}
                }
            },
        )
        .into_string(),
    )
}

fn fork_row(fork: &Fork) -> Markup {
    html! {
        tr {
            td {
                a href={ "/forks/" (fork.id) } { (fork.repo) }
                @if !fork.enabled { " " span class="pill muted" { "disabled" } }
            }
            td class="mono small muted" { (fork.upstream) }
            td class="mono small" {
                (fork.branch)
                @if let Some(up) = &fork.upstream_branch {
                    @if up != &fork.branch { span class="muted" { " ← " (up) } }
                }
            }
            td class="small muted" {
                @if fork.keep_removed.is_empty() { "—" } @else { (fork.keep_removed.len()) " path(s)" }
            }
            td {
                form method="post" action="/run" style="display:inline" {
                    input type="hidden" name="repo" value=(fork.repo);
                    input type="hidden" name="dry_run" value="1";
                    button type="submit" { "Dry run" }
                }
            }
        }
    }
}

fn run_row(run: &Run) -> Markup {
    html! {
        tr {
            td class="small mono muted" { (run.started_at.get(..16).unwrap_or(&run.started_at)) }
            td { (run.repo) @if run.dry_run { " " span class="pill muted" { "dry" } } }
            td class=(outcome_class(run.outcome.as_deref())) {
                @match &run.outcome {
                    Some(outcome) => (outcome),
                    None => { span class="dot" {} "running…" }
                }
            }
            td { a class="small" href={ "/runs/" (run.id) } { "Details" } }
        }
    }
}

pub async fn run_detail(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    match app.store().run(id) {
        Ok(Some(run)) => Html(
            shell(
                "run",
                "home",
                html! {
                    h2 { (run.repo) " — " (run.outcome.clone().unwrap_or_else(|| "running…".into())) }
                    div class="card small" {
                        div { span class="muted" { "Started " } span class="mono" { (run.started_at) } }
                        @if let Some(finished) = &run.finished_at {
                            div { span class="muted" { "Finished " } span class="mono" { (finished) } }
                        }
                        @if run.dry_run { div class="warn" { "Dry run — nothing was pushed." } }
                        @if let Some(url) = &run.pr_url {
                            div { a href=(url) { "Open the pull request" } }
                        }
                    }
                    details open {
                        summary { "Output" }
                        div class="details-body" { pre { (run.detail) } }
                    }
                    a class="btn" href="/" { "Back" }
                },
            )
            .into_string(),
        )
        .into_response(),
        Ok(None) => error_page("No such run", "It may have been pruned."),
        Err(error) => error_page("Could not read the run", &format!("{error:#}")),
    }
}

pub async fn settings(State(app): State<App>) -> impl IntoResponse {
    let settings = app.store().settings().unwrap_or_default();
    let secrets = app.store().secret_status().ok();

    Html(
        shell(
            "settings",
            "settings",
            html! {
                h2 { "Forge" }
                form method="post" action="/settings" {
                    div class="card" {
                        div class="grid2" {
                            div {
                                label for="forge_url" { "Gitea or Forgejo URL" }
                                input type="text" id="forge_url" name="forge_url"
                                      value=(settings.forge_url) placeholder="https://gitea.example.com";
                            }
                            div {
                                label for="forge_owner" { "Owner (user or organisation)" }
                                input type="text" id="forge_owner" name="forge_owner"
                                      value=(settings.forge_owner) placeholder="my-org";
                            }
                            div {
                                label for="forge_username" { "Machine account username " span class="muted" { "(optional)" } }
                                input type="text" id="forge_username" name="forge_username"
                                      value=(settings.forge_username.clone().unwrap_or_default())
                                      placeholder="weir-bot";
                            }
                            div {
                                label for="schedule" { "Schedule " span class="muted" { "(cron, server local time)" } }
                                input type="text" id="schedule" name="schedule"
                                      value=(settings.schedule.clone().unwrap_or_default())
                                      placeholder="0 5 * * 5";
                            }
                        }
                        button class="primary" type="submit" { "Save" }
                    }

                    details {
                        summary { "Branch and file names" span class="muted small" { " — rarely changed" } }
                        div class="details-body" {
                            div class="grid2" {
                                div {
                                    label for="sync_branch" { "Sync branch" }
                                    input type="text" id="sync_branch" name="sync_branch" value=(settings.sync_branch);
                                }
                                div {
                                    label for="boundary_file" { "Boundary file" }
                                    input type="text" id="boundary_file" name="boundary_file" value=(settings.boundary_file);
                                }
                            }
                            p class="small muted" {
                                "The sync branch is force-pushed on every run, so nothing you want to keep "
                                "should live there. The boundary file records which upstream commit the "
                                "fork's content matches — change its name and the next sync will think it "
                                "has never run. Both are saved with the form above."
                            }
                        }
                    }
                }

                h2 { "Credentials" }
                div class="card" {
                    p class="small muted" {
                        "Stored in the database and never shown again — the field only reports whether "
                        "one is set. That makes the database file a secret, so treat its volume as one."
                    }
                    form method="post" action="/settings/forge-token" {
                        label for="forge_token" {
                            "Gitea or Forgejo access token "
                            @match secrets.map(|s| s.forge_token) {
                                Some(true) => span class="ok" { "— set" },
                                _ => span class="warn" { "— not set" },
                            }
                        }
                        div class="row" {
                            input type="password" id="forge_token" name="token"
                                  placeholder="from Settings → Applications on your forge" style="flex:1";
                            button type="submit" { "Replace" }
                        }
                        p class="small muted" {
                            "Belongs to a machine account with write access to the forks. Used both to "
                            "push the sync branch and to open the pull request, so "
                            code { "write:repository" }
                            " is enough — it never needs admin, and it is never asked to merge anything."
                        }
                    }
                    p class="small muted" {
                        "Nothing is needed for GitHub — public upstreams are cloned anonymously."
                    }
                }

                h2 { "Notifications" }
                form method="post" action="/settings/telegram" {
                    div class="card" {
                        @let has_token = secrets.is_some_and(|s| s.telegram_token);
                        @let has_chat = settings.telegram_chat.as_deref().is_some_and(|c| !c.is_empty());
                        p class="small" {
                            @if has_token && has_chat {
                                span class="ok" { "On" }
                                span class="muted" { " — one message per fork per run, including failures." }
                            } @else if !has_token && !has_chat {
                                span class="muted" {
                                    "Off. A sync on a schedule is invisible unless it says something, so                                      this is worth setting even if you only read it once a week."
                                }
                            } @else {
                                span class="warn" { "Incomplete — nothing will be sent." }
                                span class="muted" {
                                    @if has_token { " A bot with no chat id has nowhere to send." }
                                    @else { " A chat id with no bot token has nothing to send with." }
                                }
                            }
                        }
                        div class="grid2" {
                            div {
                                label for="telegram_token" {
                                    "Telegram bot token "
                                    @if has_token { span class="ok" { "— set" } }
                                    @else { span class="muted" { "— not set" } }
                                }
                                input type="password" id="telegram_token" name="token"
                                      placeholder=(if has_token { "leave blank to keep the stored one" }
                                                   else { "from @BotFather" });
                            }
                            div {
                                label for="telegram_chat" { "Chat id" }
                                input type="text" id="telegram_chat" name="chat"
                                      value=(settings.telegram_chat.clone().unwrap_or_default())
                                      placeholder="-1001234567890";
                            }
                        }
                        p class="small muted" {
                            "Both are needed, or neither does anything. Clear the chat id to turn                              notifications off without discarding the token."
                        }
                        div style="margin-top:1rem" { button class="primary" type="submit" { "Save" } }
                    }
                }
            },
        )
        .into_string(),
    )
}

pub async fn new_fork(State(app): State<App>) -> impl IntoResponse {
    // Off the async runtime. Asking the forge uses a blocking HTTP client,
    // which owns a runtime of its own; dropping that inside an async context
    // panics the worker thread.
    let found = tokio::task::spawn_blocking(move || discover(&app))
        .await
        .unwrap_or_else(|error| Err(anyhow::anyhow!("discovery task failed: {error}")));

    Html(
        shell(
            "add fork",
            "home",
            html! {
                h2 { "Add a fork" }

                @match &found {
                    Ok(repos) if !repos.is_empty() => {
                        div class="card" {
                            p class="small muted" {
                                "Found on the forge and not yet configured. The upstream comes from "
                                "what the repository was migrated from, so it is usually already right — "
                                "check it before saving."
                            }
                            table { tbody { @for repo in repos {
                                tr {
                                    td { (repo.name) }
                                    td class="mono small muted" {
                                        @match &repo.upstream {
                                            Some(url) => (url),
                                            None => span class="warn" { "no upstream recorded — type it below" },
                                        }
                                    }
                                    td class="mono small" { (repo.default_branch) }
                                    td {
                                        form method="post" action="/forks" {
                                            input type="hidden" name="repo" value=(repo.name);
                                            input type="hidden" name="upstream" value=(repo.upstream.clone().unwrap_or_default());
                                            input type="hidden" name="branch" value=(repo.default_branch);
                                            input type="hidden" name="upstream_branch" value="";
                                            input type="hidden" name="keep_removed" value="";
                                            input type="hidden" name="enabled" value="1";
                                            button type="submit" disabled[repo.upstream.is_none()] { "Add" }
                                        }
                                    }
                                }
                            }}}
                        }
                    }
                    Ok(_) => div class="card muted small" { "Every repository on the forge is already configured." }
                    Err(error) => div class="note small" {
                        "Could not list repositories: " (format!("{error:#}"))
                        ". Add one by hand below."
                    }
                }

                details {
                    summary { "Enter one by hand" span class="muted small" { " — if it is not on the forge yet" } }
                    div class="details-body" {
                        form method="post" action="/forks" { (fork_fields(None)) }
                    }
                }
            },
        )
        .into_string(),
    )
}

pub async fn edit_fork(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    match app.store().fork(id) {
        Ok(Some(fork)) => Html(
            shell(
                "edit fork",
                "home",
                html! {
                    h2 { (fork.repo) }
                    form method="post" action={ "/forks/" (fork.id) } { (fork_fields(Some(&fork))) }
                    div class="card" {
                        form method="post" action={ "/forks/" (fork.id) "/delete" } {
                            button class="danger" type="submit" { "Remove this fork" }
                            span class="small muted" {
                                " — stops syncing it. Nothing in the repository is touched."
                            }
                        }
                    }
                },
            )
            .into_string(),
        )
        .into_response(),
        Ok(None) => error_page("No such fork", "It may already have been removed."),
        Err(error) => error_page("Could not read the fork", &format!("{error:#}")),
    }
}

fn fork_fields(fork: Option<&Fork>) -> Markup {
    let repo = fork.map(|f| f.repo.clone()).unwrap_or_default();
    let upstream = fork.map(|f| f.upstream.clone()).unwrap_or_default();
    let branch = fork.map(|f| f.branch.clone()).unwrap_or_default();
    let upstream_branch = fork
        .and_then(|f| f.upstream_branch.clone())
        .unwrap_or_default();
    let keep_removed = fork.map(|f| f.keep_removed.join("\n")).unwrap_or_default();
    let enabled = fork.map(|f| f.enabled).unwrap_or(true);

    html! {
        div class="card" {
            div class="grid2" {
                div {
                    label for="repo" { "Repository name on the forge" }
                    input type="text" id="repo" name="repo" value=(repo) placeholder="codex";
                }
                div {
                    label for="upstream" { "Upstream clone URL" }
                    input type="text" id="upstream" name="upstream" value=(upstream)
                          placeholder="https://github.com/openai/codex.git";
                }
                div {
                    label for="branch" { "Branch in your fork the sync targets" }
                    input type="text" id="branch" name="branch" value=(branch) placeholder="main";
                }
                div {
                    label for="upstream_branch" { "Branch to take from upstream " span class="muted" { "(defaults to the same)" } }
                    input type="text" id="upstream_branch" name="upstream_branch" value=(upstream_branch);
                }
            }
            label for="keep_removed" { "Paths this fork keeps removed " span class="muted" { "(one per line)" } }
            textarea id="keep_removed" name="keep_removed" placeholder=".github/workflows/release.yml" { (keep_removed) }
            p class="small muted" {
                "Paths you deleted on purpose that upstream keeps editing. Every upstream change "
                "inside them is discarded — the run and the pull request say how many, so you can "
                "see what went. Leave this empty and those conflicts come to you instead."
            }
            label class="row small" style="margin-top:.75rem" {
                input type="checkbox" name="enabled" value="1" checked[enabled] style="width:auto";
                " Include this fork when syncing"
            }
            div style="margin-top:1rem" { button class="primary" type="submit" { "Save" } }
        }
    }
}
