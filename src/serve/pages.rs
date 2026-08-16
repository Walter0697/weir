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
th,td { text-align:left; padding:.55rem .6rem; border-bottom:1px solid var(--line); }
/* Middle, not top: every row here is one line of text beside a button, and
   top-aligning the text leaves it sitting above the button's centre. */
td { vertical-align:middle; }
th { vertical-align:bottom; }
/* The action column: hard right, and never wrapped onto two lines. */
td:last-child { text-align:right; white-space:nowrap; }
td:last-child form { display:inline-block; margin:0; }
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
                    a href="/connections" class=(if current == "connections" { "on" } else { "" }) { "Connections" }
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
        // Cancelled is neither good nor bad: somebody meant it.
        Some("cancelled") => "muted",
        _ => "muted",
    }
}

pub async fn dashboard(State(app): State<App>) -> impl IntoResponse {
    let forks = app.store().forks().unwrap_or_default();
    let runs = app.store().recent_runs(15).unwrap_or_default();
    let settings = app.store().settings().unwrap_or_default();
    let connections = app.store().connections().unwrap_or_default();

    Html(
        shell(
            "forks",
            "home",
            html! {
                @if connections.is_empty() {
                    div class="note" {
                        "No " a href="/connections" { "connections" }
                        " yet — add the forge your forks live on before adding a fork."
                    }
                } @else if connections.iter().any(|c| !c.has_token) {
                    div class="note" {
                        "A " a href="/connections" { "connection" }
                        " has no token. Pushing, listing, and opening pull requests all need one."
                    }
                }

                div class="row" style="justify-content:space-between; margin-bottom:.5rem" {
                    h2 style="margin:0" { "Forks" }
                    div class="row" {
                        @let running = runs.iter().any(|r| r.finished_at.is_none());
                        @if running {
                            form method="post" action="/cancel" style="display:inline" {
                                button class="danger" type="submit" { "Stop" }
                            }
                        }
                        a class="btn" href="/forks/new" { "Add fork" }
                        form method="post" action="/run" style="display:inline" {
                            input type="hidden" name="dry_run" value="1";
                            button type="submit" { "Dry run all" }
                        }
                        form method="post" action="/run" style="display:inline" {
                            button class="primary" type="submit" disabled[running] { "Sync all" }
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
                a href={ "/forks/" (fork.id) } { (fork.owner) "/" (fork.repo) }
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
    let has_token = secrets.is_some_and(|s| s.telegram_token);
    let has_chat = settings
        .telegram_chat
        .as_deref()
        .is_some_and(|c| !c.is_empty());

    Html(
        shell(
            "settings",
            "settings",
            html! {
                h2 { "Schedule" }
                form method="post" action="/settings" {
                    div class="card" {
                        label for="schedule" { "Cron expression " span class="muted" { "(server local time; blank to disable)" } }
                        input type="text" id="schedule" name="schedule"
                              value=(settings.schedule.clone().unwrap_or_default())
                              placeholder="0 5 * * 5";
                        p class="small muted" {
                            "Every enabled fork syncs when this comes round. Leave it empty and "
                            "nothing runs unless you press a button."
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
                                    "The sync branch is force-pushed on every run, so nothing you want to "
                                    "keep should live there. The boundary file records which upstream "
                                    "commit each fork's content matches — rename it and the next sync "
                                    "will think it has never run."
                                }
                            }
                        }

                        div style="margin-top:1rem" { button class="primary" type="submit" { "Save" } }
                    }
                }

                h2 { "Notifications" }
                form method="post" action="/settings/telegram" {
                    div class="card" {
                        p class="small" {
                            @if has_token && has_chat {
                                span class="ok" { "On" }
                                span class="muted" { " — one message per fork per run, including failures." }
                            } @else if !has_token && !has_chat {
                                span class="muted" {
                                    "Off. A sync on a schedule is invisible unless it says something, so \
                                     this is worth setting even if you only read it once a week."
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
                            "Both are needed, or neither does anything. Clear the chat id to turn \
                             notifications off without discarding the token."
                        }
                        div style="margin-top:1rem" { button class="primary" type="submit" { "Save" } }
                    }
                }
            },
        )
        .into_string(),
    )
}

pub async fn connections(State(app): State<App>) -> impl IntoResponse {
    let connections = app.store().connections().unwrap_or_default();
    let forks = app.store().forks().unwrap_or_default();

    Html(
        shell(
            "connections",
            "connections",
            html! {
                h2 { "Connections" }
                p class="small muted" {
                    "A forge and the credential for it. They are one thing rather than two: the URL "
                    "cannot reach a private repository without the token, and the token means nothing "
                    "without the URL it belongs to. Add as many as you have."
                }

                @if connections.is_empty() {
                    div class="note" {
                        "Nothing here yet. Add the forge your forks live on, then add the forks."
                    }
                }

                @for connection in &connections {
                    @let in_use = forks.iter().filter(|f| f.connection_id == connection.id).count();
                    details {
                        summary {
                            strong { (connection.name) }
                            span class="muted small" { " — " (connection.url) }
                            @if connection.has_token { span class="ok small" { " · token set" } }
                            @else { span class="warn small" { " · no token" } }
                            span class="muted small" {
                                " · " (in_use) " fork(s)"
                            }
                        }
                        div class="details-body" {
                            form method="post" action={ "/connections/" (connection.id) } {
                                (connection_fields(Some(connection)))
                                button class="primary" type="submit" { "Save" }
                            }
                            form method="post" action={ "/connections/" (connection.id) "/delete" }
                                 style="margin-top:.75rem" {
                                button class="danger" type="submit" { "Remove" }
                                span class="small muted" {
                                    @if in_use > 0 {
                                        " — " (in_use) " fork(s) still use this, so removing it is refused."
                                    } @else {
                                        " — nothing uses this."
                                    }
                                }
                            }
                        }
                    }
                }

                h2 { "Add a connection" }
                form method="post" action="/connections" {
                    div class="card" {
                        (connection_fields(None))
                        button class="primary" type="submit" { "Add" }
                    }
                }
            },
        )
        .into_string(),
    )
}

fn connection_fields(connection: Option<&crate::store::Connection>) -> Markup {
    let name = connection.map(|c| c.name.clone()).unwrap_or_default();
    let url = connection.map(|c| c.url.clone()).unwrap_or_default();
    let username = connection
        .and_then(|c| c.username.clone())
        .unwrap_or_default();
    let kind = connection
        .map(|c| c.kind.clone())
        .unwrap_or_else(|| "gitea".into());
    let has_token = connection.is_some_and(|c| c.has_token);

    html! {
        div class="grid2" {
            div {
                label { "Name " span class="muted" { "(how forks refer to it)" } }
                input type="text" name="name" value=(name) placeholder="home gitea";
            }
            div {
                label { "Kind" }
                select name="kind" {
                    option value="gitea" selected[kind == "gitea"] { "Gitea" }
                    option value="forgejo" selected[kind == "forgejo"] { "Forgejo" }
                }
            }
            div {
                label { "URL" }
                input type="text" name="url" value=(url) placeholder="https://gitea.example.com";
            }
            div {
                label { "Machine account username " span class="muted" { "(optional)" } }
                input type="text" name="username" value=(username) placeholder="weir-bot";
            }
        }
        label {
            "Access token "
            @if has_token { span class="ok" { "— set" } } @else { span class="warn" { "— required" } }
        }
        input type="password" name="token"
              placeholder=(if has_token { "leave blank to keep the stored one" }
                           else { "write:repository scope is enough" });
        p class="small muted" {
            "Used to push the sync branch and to open the pull request, so "
            code { "write:repository" }
            " is enough — it never needs admin and is never asked to merge. Stored in the "
            "database and never shown again, which makes that file a secret."
        }
    }
}

pub async fn new_fork(
    State(app): State<App>,
    axum::extract::Query(query): axum::extract::Query<super::DiscoverQuery>,
) -> impl IntoResponse {
    let connections = app.store().connections().unwrap_or_default();
    let chosen = query
        .connection
        .or_else(|| connections.first().map(|c| c.id));
    let owner = query.owner.clone().unwrap_or_default();

    // Off the async runtime. Asking the forge uses a blocking HTTP client,
    // which owns a runtime of its own; dropping that inside an async context
    // panics the worker thread.
    let found = match (chosen, owner.is_empty()) {
        (Some(id), false) => {
            let app = app.clone();
            let owner = owner.clone();
            Some(
                tokio::task::spawn_blocking(move || discover(&app, id, &owner))
                    .await
                    .unwrap_or_else(|error| Err(anyhow::anyhow!("discovery failed: {error}"))),
            )
        }
        _ => None,
    };

    Html(
        shell(
            "add fork",
            "home",
            html! {
                @if connections.is_empty() {
                    div class="note" {
                        "Add a " a href="/connections" { "connection" }
                        " first — a fork has to live on a forge weir can reach."
                    }
                } @else {
                    h2 { "Find them on the forge" }
                    form method="get" action="/forks/new" {
                        div class="card" {
                            div class="grid2" {
                                div {
                                    label { "Connection" }
                                    select name="connection" {
                                        @for connection in &connections {
                                            option value=(connection.id) selected[Some(connection.id) == chosen] {
                                                (connection.name)
                                            }
                                        }
                                    }
                                }
                                div {
                                    label { "Owner " span class="muted" { "(user or organisation)" } }
                                    input type="text" name="owner" value=(owner) placeholder="my-org";
                                }
                            }
                            div style="margin-top:1rem" { button type="submit" { "List repositories" } }
                        }
                    }

                    @match &found {
                        None => div class="card muted small" {
                            "Pick a connection and an owner, then list what is there."
                        }
                        Some(Ok(repos)) if repos.is_empty() => {
                            @let already = app.store().forks().unwrap_or_default().iter()
                                .filter(|f| Some(f.connection_id) == chosen && f.owner == owner)
                                .count();
                            div class="card muted small" {
                                @if already > 0 {
                                    "Nothing left under that owner — all " (already)
                                    " repository(s) there are already configured."
                                } @else {
                                    "No repositories visible under that owner. Check the spelling, and "
                                    "that the account behind this connection's token can see them — a "
                                    "token with no access to an organisation sees an empty list rather "
                                    "than an error."
                                }
                            }
                        }
                        Some(Ok(repos)) => div class="card" {
                            p class="small muted" {
                                "The upstream comes from what each repository was migrated from, so it "
                                "is usually already right. Check it before saving."
                            }
                            table { tbody { @for repo in repos {
                                tr {
                                    td { (repo.name) }
                                    td class="mono small muted" {
                                        @match &repo.upstream {
                                            Some(url) => (url),
                                            None => span class="warn" { "no upstream recorded — add it by hand" },
                                        }
                                    }
                                    td class="mono small" { (repo.default_branch) }
                                    td {
                                        form method="post" action="/forks" {
                                            input type="hidden" name="connection_id" value=(chosen.unwrap_or_default());
                                            input type="hidden" name="owner" value=(owner);
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
                        Some(Err(error)) => div class="note small" {
                            "Could not list repositories: " (format!("{error:#}"))
                        }
                    }

                    details {
                        summary { "Enter one by hand" span class="muted small" { " — if it is not on the forge yet" } }
                        div class="details-body" {
                            form method="post" action="/forks" { (fork_fields(None, &connections)) }
                        }
                    }
                }
            },
        )
        .into_string(),
    )
}

pub async fn edit_fork(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    let connections = app.store().connections().unwrap_or_default();
    match app.store().fork(id) {
        Ok(Some(fork)) => Html(
            shell(
                "edit fork",
                "home",
                html! {
                    h2 { (fork.owner) "/" (fork.repo) }
                    form method="post" action={ "/forks/" (fork.id) } { (fork_fields(Some(&fork), &connections)) }
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

fn fork_fields(fork: Option<&Fork>, connections: &[crate::store::Connection]) -> Markup {
    let owner = fork.map(|f| f.owner.clone()).unwrap_or_default();
    let repo = fork.map(|f| f.repo.clone()).unwrap_or_default();
    let upstream = fork.map(|f| f.upstream.clone()).unwrap_or_default();
    let branch = fork.map(|f| f.branch.clone()).unwrap_or_default();
    let upstream_branch = fork
        .and_then(|f| f.upstream_branch.clone())
        .unwrap_or_default();
    let keep_removed = fork.map(|f| f.keep_removed.join("\n")).unwrap_or_default();
    let enabled = fork.map(|f| f.enabled).unwrap_or(true);
    let chosen = fork.map(|f| f.connection_id);

    html! {
        div class="card" {
            div class="grid2" {
                div {
                    label { "Connection " span class="muted" { "(which forge it lives on)" } }
                    select name="connection_id" {
                        @for connection in connections {
                            option value=(connection.id) selected[Some(connection.id) == chosen] {
                                (connection.name)
                            }
                        }
                    }
                }
                div {
                    label { "Owner " span class="muted" { "(user or organisation)" } }
                    input type="text" name="owner" value=(owner) placeholder="my-org";
                }
                div {
                    label { "Repository name" }
                    input type="text" name="repo" value=(repo) placeholder="codex";
                }
                div {
                    label { "Upstream clone URL" }
                    input type="text" name="upstream" value=(upstream)
                          placeholder="https://github.com/openai/codex.git";
                }
                div {
                    label { "Branch in your fork the sync targets" }
                    input type="text" name="branch" value=(branch) placeholder="main";
                }
                div {
                    label { "Branch to take from upstream " span class="muted" { "(defaults to the same)" } }
                    input type="text" name="upstream_branch" value=(upstream_branch);
                }
            }
            label { "Paths this fork keeps removed " span class="muted" { "(one per line)" } }
            textarea name="keep_removed" placeholder=".github/workflows/release.yml" { (keep_removed) }
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
