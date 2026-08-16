//! The web UI, and the scheduler that goes with it.
//!
//! `weir run --config` and `weir serve` are two front ends to the same library,
//! and they never share a source of truth: `run` reads a TOML file and does not
//! open the database, `serve` reads the database and ignores `--config`. There
//! is always exactly one answer to where a setting came from.
//!
//! Bound to loopback by default, because this is a control plane — anything
//! that reaches it can change which repositories get force-pushed. Put it
//! behind whatever already fronts your other services if you want it elsewhere.

mod auth;
mod pages;

use crate::forge::Forge;
use crate::git::Cancel;
use crate::notify::{self, Notifier};
use crate::runner::{self, ForgeSpec, ForkSpec, Options};
use crate::store::{Fork, NewConnection, NewFork, NewWatch, Settings, Store, Watch};
use crate::watch::{self, Skipped};
use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct App {
    store: Arc<Store>,
    /// The stop switch for whatever batch is running. Runs are serial, so one
    /// flag is enough; it is replaced at the start of each batch rather than
    /// reset, so a stale cancel cannot carry into the next one.
    cancel: Arc<std::sync::Mutex<Cancel>>,
}

pub async fn serve(store: Store, addr: SocketAddr) -> Result<()> {
    let app = App {
        store: Arc::new(store),
        cancel: Arc::new(std::sync::Mutex::new(Cancel::new())),
    };

    let scheduler = app.clone();
    tokio::spawn(async move { schedule_loop(scheduler).await });

    let gate = auth::Auth::from_env();
    let router = Router::new()
        .route("/", get(pages::dashboard))
        .route("/settings", get(pages::settings).post(save_settings))
        .route("/settings/telegram", post(save_telegram))
        .route(
            "/connections",
            get(pages::connections).post(create_connection),
        )
        .route("/connections/{id}", post(update_connection))
        .route("/connections/{id}/delete", post(delete_connection))
        .route("/forks/new", get(pages::new_fork))
        .route("/forks", post(create_fork))
        .route("/forks/{id}", get(pages::edit_fork).post(update_fork))
        .route("/forks/{id}/delete", post(delete_fork))
        .route("/watches", get(pages::watches).post(create_watch))
        .route("/watches/{id}", post(update_watch))
        .route("/watches/{id}/delete", post(delete_watch))
        .route("/watches/{id}/except", post(except_repo))
        .route("/watches/{id}/include", post(include_repo))
        .route("/run", post(trigger_run))
        .route("/cancel", post(cancel_run))
        .route("/runs/{id}", get(pages::run_detail))
        .with_state(app)
        .route("/login", get(auth::login_page).post(auth::login))
        .layer(axum::middleware::from_fn_with_state(
            gate.clone(),
            auth::guard,
        ))
        .with_state(gate.clone());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    println!("weir: listening on http://{addr}");
    match (addr.ip().is_loopback(), gate.required()) {
        (true, false) => println!("weir: loopback only, so no access token is required"),
        (_, true) => println!("weir: an access token is required (WEIR_UI_TOKEN)"),
        (false, false) => println!(
            "weir: WARNING — bound to {} with no access token. Anything that can reach this \
             can change which repositories get force-pushed and trigger runs. Set WEIR_UI_TOKEN, \
             or publish it to 127.0.0.1 only.",
            addr.ip()
        ),
    }
    axum::serve(listener, router)
        .await
        .context("serving the web UI")?;
    Ok(())
}

impl App {
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Everything a sync needs, assembled from the fork's own connection.
    ///
    /// Resolved per fork rather than globally, which is what lets two forks sit
    /// on different forges, or under different owners on the same one.
    fn forge_spec(&self, fork: &Planned) -> Result<ForgeSpec> {
        let connection = self
            .store
            .connection(fork.connection_id)?
            .with_context(|| format!("{} has no connection; pick one for it", fork.repo))?;
        Ok(ForgeSpec {
            url: connection.url,
            owner: fork.owner.clone(),
            username: connection.username,
            token: self.store.connection_token(fork.connection_id)?,
        })
    }

    fn options(&self, dry_run: bool) -> Result<Options> {
        let settings = self.store.settings()?;
        Ok(Options {
            sync_branch: settings.sync_branch,
            boundary_file: settings.boundary_file,
            dry_run,
        })
    }

    fn notifiers(&self) -> Vec<Box<dyn Notifier>> {
        let mut notifiers: Vec<Box<dyn Notifier>> = Vec::new();
        if let (Ok(Some(token)), Ok(settings)) =
            (self.store.telegram_token(), self.store.settings())
        {
            if let Some(chat) = settings.telegram_chat.filter(|c| !c.trim().is_empty()) {
                match notify::telegram::Telegram::new(token, chat) {
                    Ok(telegram) => notifiers.push(Box::new(telegram)),
                    Err(error) => eprintln!("weir: telegram unusable: {error:#}"),
                }
            }
        }
        notifiers
    }

    /// Runs one fork and records it. Blocking on purpose — the git and HTTP
    /// work underneath is blocking, so it must never run on the async runtime.
    fn sync_one_blocking(&self, fork: Planned, dry_run: bool, cancel: &Cancel) {
        let run_id = match self.store.start_run(&fork.repo, dry_run) {
            Ok(id) => id,
            Err(error) => {
                eprintln!("weir: could not record a run: {error:#}");
                return;
            }
        };

        let spec = ForkSpec {
            repo: fork.repo.clone(),
            upstream: fork.upstream.clone(),
            branch: fork.branch.clone(),
            upstream_branch: fork.upstream_branch.clone(),
            keep_removed: fork.keep_removed.clone(),
        };

        let result = self
            .forge_spec(&fork)
            .and_then(|forge| Ok((forge, self.options(dry_run)?)))
            .and_then(|(forge, options)| runner::sync_fork(&forge, &spec, &options, cancel));

        let notifiers = self.notifiers();
        match result {
            Ok(report) => {
                let _ = self.store.finish_run(
                    run_id,
                    report.outcome.label(),
                    &report.text(),
                    report.pr_url.as_deref(),
                );
                notify::announce(
                    &notifiers,
                    &notify::summarise(&fork.repo, &report.sync, report.pr_url.as_deref(), dry_run),
                );
            }
            Err(error) if crate::git::was_cancelled(&error) => {
                // Not a failure, and not worth a notification: somebody pressed
                // the button and is looking at the page.
                let _ = self
                    .store
                    .finish_run(run_id, "cancelled", "Stopped on request.", None);
            }
            Err(error) => {
                let detail = format!("{error:#}");
                let _ = self.store.finish_run(run_id, "failed", &detail, None);
                notify::announce(
                    &notifiers,
                    &format!("❌ {}: sync failed — {detail}", fork.repo),
                );
            }
        }
    }
}

/// One repository a run will actually touch, however it got there.
#[derive(Debug, Clone)]
pub struct Planned {
    pub connection_id: i64,
    pub owner: String,
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
}

impl From<Fork> for Planned {
    fn from(fork: Fork) -> Self {
        Self {
            connection_id: fork.connection_id,
            owner: fork.owner,
            repo: fork.repo,
            upstream: fork.upstream,
            branch: fork.branch,
            upstream_branch: fork.upstream_branch,
            keep_removed: fork.keep_removed,
        }
    }
}

/// What a watch currently covers, and what it leaves alone and why.
pub struct Expansion {
    pub covered: Vec<Planned>,
    pub skipped: Vec<(String, Skipped)>,
}

/// Expands one watch against the forge as it is right now.
///
/// Deliberately not cached: the whole point of a watch is that it notices a
/// repository nobody told it about, and a cached answer would not.
pub fn expand(app: &App, watch: &Watch) -> Result<Expansion> {
    let connection = app
        .store
        .connection(watch.connection_id)?
        .context("that watch points at a connection that no longer exists")?;
    let token = app
        .store
        .connection_token(watch.connection_id)?
        .context("that connection has no token, so the forge cannot be listed")?;
    let gitea = crate::forge::gitea::Gitea::new(&connection.url, &watch.owner, &token)?;

    let configured: Vec<String> = app
        .store
        .forks()?
        .into_iter()
        .filter(|f| f.connection_id == watch.connection_id && f.owner == watch.owner)
        .map(|f| f.repo)
        .collect();

    let mut covered = Vec::new();
    let mut skipped = Vec::new();
    for repo in gitea.discover()? {
        if let Some(pattern) = watch
            .except
            .iter()
            .find(|p| watch::excluded(&repo.name, std::slice::from_ref(p)))
        {
            skipped.push((repo.name, Skipped::Excepted(pattern.clone())));
            continue;
        }
        // A hand-written fork wins, so watching an owner and tuning one of its
        // repositories are not in competition.
        if configured.contains(&repo.name) {
            skipped.push((repo.name, Skipped::ConfiguredSeparately));
            continue;
        }
        let Some(upstream) = repo.upstream else {
            skipped.push((repo.name, Skipped::NoUpstream));
            continue;
        };
        covered.push(Planned {
            connection_id: watch.connection_id,
            owner: watch.owner.clone(),
            repo: repo.name,
            upstream,
            branch: repo.default_branch,
            upstream_branch: None,
            keep_removed: Vec::new(),
        });
    }
    Ok(Expansion { covered, skipped })
}

/// Everything a run should touch: the forks written down, plus whatever the
/// watches cover right now.
pub fn planned(app: &App, only: Option<&str>) -> Result<Vec<Planned>> {
    let mut targets: Vec<Planned> = app
        .store
        .forks()?
        .into_iter()
        .filter(|f| f.enabled)
        .map(Planned::from)
        .collect();

    for watch in app.store.watches()?.into_iter().filter(|w| w.enabled) {
        match expand(app, &watch) {
            Ok(expansion) => targets.extend(expansion.covered),
            // A watch that cannot be listed must not stop the forks that can.
            Err(error) => eprintln!("weir: could not expand {}: {error:#}", watch.owner),
        }
    }

    Ok(match only {
        Some(name) => targets.into_iter().filter(|t| t.repo == name).collect(),
        None => targets,
    })
}

#[derive(Deserialize)]
pub struct SettingsForm {
    sync_branch: String,
    boundary_file: String,
    schedule: String,
}

async fn save_settings(
    State(app): State<App>,
    axum::Form(form): axum::Form<SettingsForm>,
) -> impl IntoResponse {
    let schedule = blank_to_none(&form.schedule);
    // Refuse a schedule that will never fire rather than accepting it and
    // going quiet — a cron typo is otherwise indistinguishable from a job that
    // simply has not come round yet.
    if let Some(expression) = &schedule {
        if let Err(error) = parse_cron(expression) {
            return pages::error_page("That schedule is not a valid cron expression", &error);
        }
    }

    let settings = Settings {
        sync_branch: form.sync_branch.trim().to_string(),
        boundary_file: form.boundary_file.trim().to_string(),
        schedule,
        // Carried through untouched: notifications are edited by their own
        // form, so saving the forge settings must not blank the chat id.
        telegram_chat: app.store.settings().ok().and_then(|s| s.telegram_chat),
    };
    match app.store.save_settings(&settings) {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(error) => pages::error_page("Could not save settings", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct ConnectionForm {
    name: String,
    kind: String,
    url: String,
    username: String,
    #[serde(default)]
    token: String,
}

impl ConnectionForm {
    fn split(self) -> (NewConnection, String) {
        (
            NewConnection {
                name: self.name.trim().to_string(),
                kind: self.kind.trim().to_string(),
                url: self.url.trim().trim_end_matches('/').to_string(),
                username: blank_to_none(&self.username),
            },
            self.token,
        )
    }
}

fn check_connection(new: &NewConnection) -> Option<String> {
    if new.name.is_empty() {
        return Some("Give it a name, so forks can say which one they use.".into());
    }
    if !new.url.starts_with("http://") && !new.url.starts_with("https://") {
        return Some(format!(
            "The URL must start with http:// or https:// — got {:?}.",
            new.url
        ));
    }
    if new.url.starts_with("http://") {
        // Not refused: a homelab forge on a trusted network is a legitimate
        // setup. Said out loud, because the token crosses that network in the
        // clear on every run.
        return None;
    }
    None
}

async fn create_connection(
    State(app): State<App>,
    axum::Form(form): axum::Form<ConnectionForm>,
) -> impl IntoResponse {
    let (new, token) = form.split();
    if let Some(problem) = check_connection(&new) {
        return pages::error_page("That connection cannot be added", &problem);
    }
    if token.trim().is_empty() {
        return pages::error_page(
            "That connection cannot be added",
            "A token is required: without one the forge cannot be listed, pushed to, \
             or asked to open a pull request.",
        );
    }
    match app.store.add_connection(&new, token.trim()) {
        Ok(_) => Redirect::to("/connections").into_response(),
        Err(error) => pages::error_page("Could not add the connection", &format!("{error:#}")),
    }
}

async fn update_connection(
    State(app): State<App>,
    Path(id): Path<i64>,
    axum::Form(form): axum::Form<ConnectionForm>,
) -> impl IntoResponse {
    let (new, token) = form.split();
    if let Some(problem) = check_connection(&new) {
        return pages::error_page("That change cannot be saved", &problem);
    }
    match app.store.update_connection(id, &new, &token) {
        Ok(()) => Redirect::to("/connections").into_response(),
        Err(error) => pages::error_page("Could not save the connection", &format!("{error:#}")),
    }
}

async fn delete_connection(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    // Refused rather than cascaded: removing a connection out from under a fork
    // would leave it configured but unsyncable, and silently.
    match app.store.forks_using(id) {
        Ok(0) => {}
        Ok(n) => {
            return pages::error_page(
                "That connection is still in use",
                &format!(
                    "{n} fork(s) sync through it. Point them at another connection, or remove \
                     them first."
                ),
            )
        }
        Err(error) => return pages::error_page("Could not check the forks", &format!("{error:#}")),
    }
    match app.store.delete_connection(id) {
        Ok(()) => Redirect::to("/connections").into_response(),
        Err(error) => pages::error_page("Could not remove the connection", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct TelegramForm {
    token: String,
    chat: String,
}

/// Both halves at once, because either alone does nothing: a bot with no chat
/// has nowhere to send, and a chat with no bot has nothing to send with.
async fn save_telegram(
    State(app): State<App>,
    axum::Form(form): axum::Form<TelegramForm>,
) -> impl IntoResponse {
    let mut settings = match app.store.settings() {
        Ok(settings) => settings,
        Err(error) => return pages::error_page("Could not read settings", &format!("{error:#}")),
    };
    settings.telegram_chat = blank_to_none(&form.chat);
    if let Err(error) = app.store.save_settings(&settings) {
        return pages::error_page("Could not save the chat id", &format!("{error:#}"));
    }
    // A blank token leaves the stored one alone, so the chat id can be edited
    // without pasting the token again — it is never shown, so it cannot be
    // round-tripped through the form.
    if !form.token.trim().is_empty() {
        if let Err(error) = app.store.set_telegram_token(form.token.trim()) {
            return pages::error_page("Could not store the token", &format!("{error:#}"));
        }
    }
    Redirect::to("/settings").into_response()
}

#[derive(Deserialize)]
pub struct ForkForm {
    connection_id: i64,
    owner: String,
    repo: String,
    upstream: String,
    branch: String,
    upstream_branch: String,
    keep_removed: String,
    #[serde(default)]
    enabled: Option<String>,
}

impl ForkForm {
    fn into_new(self) -> NewFork {
        NewFork {
            connection_id: self.connection_id,
            owner: self.owner.trim().to_string(),
            repo: self.repo.trim().to_string(),
            upstream: self.upstream.trim().to_string(),
            branch: self.branch.trim().to_string(),
            upstream_branch: blank_to_none(&self.upstream_branch),
            keep_removed: self
                .keep_removed
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            enabled: self.enabled.is_some(),
        }
    }
}

async fn create_fork(
    State(app): State<App>,
    axum::Form(form): axum::Form<ForkForm>,
) -> impl IntoResponse {
    let fork = form.into_new();
    if let Some(problem) = check_fork(&app, &fork) {
        return pages::error_page("That fork cannot be added", &problem);
    }
    match app.store.add_fork(&fork) {
        Ok(_) => Redirect::to("/").into_response(),
        Err(error) => pages::error_page("Could not add the fork", &format!("{error:#}")),
    }
}

async fn update_fork(
    State(app): State<App>,
    Path(id): Path<i64>,
    axum::Form(form): axum::Form<ForkForm>,
) -> impl IntoResponse {
    let fork = form.into_new();
    if let Some(problem) = check_fork(&app, &fork) {
        return pages::error_page("That change cannot be saved", &problem);
    }
    match app.store.update_fork(id, &fork) {
        Ok(()) => Redirect::to("/").into_response(),
        Err(error) => pages::error_page("Could not save the fork", &format!("{error:#}")),
    }
}

async fn delete_fork(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    match app.store.delete_fork(id) {
        Ok(()) => Redirect::to("/").into_response(),
        Err(error) => pages::error_page("Could not remove the fork", &format!("{error:#}")),
    }
}

/// The one check worth making before anything is stored: a fork must not target
/// the branch that gets force-pushed every run.
fn check_fork(app: &App, fork: &NewFork) -> Option<String> {
    if fork.repo.is_empty() {
        return Some("The repository name is empty.".into());
    }
    if fork.owner.is_empty() {
        return Some(
            "The owner is empty — that is the user or organisation the fork lives under.".into(),
        );
    }
    match app.store.connection(fork.connection_id) {
        Ok(Some(_)) => {}
        _ => return Some("Pick a connection: that is the forge this fork lives on.".into()),
    }
    if fork.upstream.is_empty() {
        return Some("The upstream URL is empty.".into());
    }
    if fork.branch.is_empty() {
        return Some("The target branch is empty.".into());
    }
    let sync_branch = app
        .store
        .settings()
        .map(|s| s.sync_branch)
        .unwrap_or_default();
    if fork.branch == sync_branch {
        return Some(format!(
            "The target branch is {sync_branch:?}, which is the sync branch itself. \
             It is force-pushed on every run, so syncing into it would destroy the fork."
        ));
    }
    None
}

#[derive(Deserialize)]
pub struct WatchForm {
    connection_id: i64,
    owner: String,
    except: String,
    #[serde(default)]
    enabled: Option<String>,
}

impl WatchForm {
    fn into_new(self) -> NewWatch {
        NewWatch {
            connection_id: self.connection_id,
            owner: self.owner.trim().to_string(),
            except: self
                .except
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            enabled: self.enabled.is_some(),
        }
    }
}

async fn create_watch(
    State(app): State<App>,
    axum::Form(form): axum::Form<WatchForm>,
) -> impl IntoResponse {
    let new = form.into_new();
    if new.owner.is_empty() {
        return pages::error_page("That watch cannot be added", "The owner is empty.");
    }
    match app.store.add_watch(&new) {
        Ok(_) => Redirect::to("/watches").into_response(),
        Err(error) => pages::error_page("Could not add the watch", &format!("{error:#}")),
    }
}

async fn update_watch(
    State(app): State<App>,
    Path(id): Path<i64>,
    axum::Form(form): axum::Form<WatchForm>,
) -> impl IntoResponse {
    match app.store.update_watch(id, &form.into_new()) {
        Ok(()) => Redirect::to("/watches").into_response(),
        Err(error) => pages::error_page("Could not save the watch", &format!("{error:#}")),
    }
}

async fn delete_watch(State(app): State<App>, Path(id): Path<i64>) -> impl IntoResponse {
    match app.store.delete_watch(id) {
        Ok(()) => Redirect::to("/watches").into_response(),
        Err(error) => pages::error_page("Could not remove the watch", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct RepoForm {
    repo: String,
}

/// Adds one repository to a watch's exceptions, by name.
async fn except_repo(
    State(app): State<App>,
    Path(id): Path<i64>,
    axum::Form(form): axum::Form<RepoForm>,
) -> impl IntoResponse {
    let Ok(Some(watch)) = app.store.watch(id) else {
        return pages::error_page("No such watch", "It may already have been removed.");
    };
    let mut except = watch.except.clone();
    let name = form.repo.trim().to_string();
    if !except.contains(&name) {
        except.push(name);
    }
    save_except(&app, id, &watch, except)
}

/// Removes a repository's own name from the exceptions.
///
/// Only ever removes an exact match. A wildcard covers repositories other than
/// this one, so quietly deleting it here would change what several of them do
/// on the strength of a button pressed next to one.
async fn include_repo(
    State(app): State<App>,
    Path(id): Path<i64>,
    axum::Form(form): axum::Form<RepoForm>,
) -> impl IntoResponse {
    let Ok(Some(watch)) = app.store.watch(id) else {
        return pages::error_page("No such watch", "It may already have been removed.");
    };
    let name = form.repo.trim();
    let except: Vec<String> = watch
        .except
        .iter()
        .filter(|pattern| pattern.trim() != name)
        .cloned()
        .collect();
    save_except(&app, id, &watch, except)
}

fn save_except(app: &App, id: i64, watch: &Watch, except: Vec<String>) -> Response {
    let updated = NewWatch {
        connection_id: watch.connection_id,
        owner: watch.owner.clone(),
        except,
        enabled: watch.enabled,
    };
    match app.store.update_watch(id, &updated) {
        Ok(()) => Redirect::to("/watches").into_response(),
        Err(error) => pages::error_page("Could not save the exceptions", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct RunRequest {
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    dry_run: Option<String>,
}

async fn trigger_run(
    State(app): State<App>,
    axum::Form(request): axum::Form<RunRequest>,
) -> impl IntoResponse {
    let dry_run = request.dry_run.is_some();
    // Watches are expanded here rather than stored, so a repository added to
    // the forge since the last run is included without anyone doing anything.
    let selected = match planned(&app, request.repo.as_deref()) {
        Ok(targets) => targets,
        Err(error) => {
            return pages::error_page("Could not work out what to sync", &format!("{error:#}"))
        }
    };

    // A fresh flag per batch, so pressing stop on one run cannot silently kill
    // the next one somebody starts.
    let cancel = Cancel::new();
    *app.cancel
        .lock()
        .expect("the cancel lock is never poisoned") = cancel.clone();

    // Handed to a blocking thread and not waited on: a sync takes minutes, and
    // a browser request that hangs that long is one the user will reload,
    // starting a second sync on top of the first.
    let worker = app.clone();
    tokio::task::spawn_blocking(move || {
        for fork in selected {
            // Between repositories as well as inside them, so stopping a batch
            // of five does not sync the remaining four first.
            if cancel.is_cancelled() {
                break;
            }
            worker.sync_one_blocking(fork, dry_run, &cancel);
        }
    });

    Redirect::to("/").into_response()
}

/// Asks whatever is running to stop.
///
/// Cooperative: it kills the git child process that is running now and stops
/// before the next repository. Anything already pushed stays pushed, which
/// costs nothing — the branch is rebuilt from scratch on the next run.
async fn cancel_run(State(app): State<App>) -> impl IntoResponse {
    app.cancel
        .lock()
        .expect("the cancel lock is never poisoned")
        .cancel();
    Redirect::to("/").into_response()
}

#[derive(Deserialize)]
pub struct DiscoverQuery {
    #[serde(default)]
    pub connection: Option<i64>,
    #[serde(default)]
    pub owner: Option<String>,
}

/// Repositories under one owner on one connection that are not yet configured.
pub fn discover(
    app: &App,
    connection_id: i64,
    owner: &str,
) -> Result<Vec<crate::forge::Discovered>> {
    let connection = app
        .store
        .connection(connection_id)?
        .context("no such connection")?;
    let token = app
        .store
        .connection_token(connection_id)?
        .context("that connection has no token, so the forge cannot be asked what it has")?;
    let gitea = crate::forge::gitea::Gitea::new(&connection.url, owner, &token)?;
    let known: Vec<String> = app
        .store
        .forks()?
        .into_iter()
        .filter(|f| f.connection_id == connection_id && f.owner == owner)
        .map(|f| f.repo)
        .collect();
    Ok(gitea
        .discover()?
        .into_iter()
        .filter(|repo| !known.contains(&repo.name))
        .collect())
}

pub fn parse_cron(expression: &str) -> std::result::Result<croner::Cron, String> {
    croner::Cron::new(expression)
        .parse()
        .map_err(|error| error.to_string())
}

/// Fires the schedule when it comes round.
///
/// Checked once a minute against the wall clock rather than slept precisely:
/// the schedule can change under it from the UI, and a loop that has already
/// committed to sleeping until Friday cannot notice.
async fn schedule_loop(app: App) {
    let mut last_fired = String::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        let Ok(settings) = app.store.settings() else {
            continue;
        };
        let Some(expression) = settings.schedule.filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let Ok(cron) = parse_cron(&expression) else {
            continue;
        };

        let now = chrono::Local::now();
        // Minute resolution, and a marker so a schedule due at 05:00 fires once
        // rather than on both checks inside that minute.
        let stamp = now.format("%Y-%m-%dT%H:%M").to_string();
        if stamp == last_fired {
            continue;
        }
        if !cron.is_time_matching(&now).unwrap_or(false) {
            continue;
        }
        last_fired = stamp;

        let Ok(forks) = planned(&app, None) else {
            continue;
        };
        let cancel = Cancel::new();
        *app.cancel
            .lock()
            .expect("the cancel lock is never poisoned") = cancel.clone();
        let worker = app.clone();
        tokio::task::spawn_blocking(move || {
            for fork in forks {
                if cancel.is_cancelled() {
                    break;
                }
                worker.sync_one_blocking(fork, false, &cancel);
            }
        });
    }
}

fn blank_to_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Only used to keep the query extractor honest in the router.
pub type DiscoverParams = Query<DiscoverQuery>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_cron_expression_is_accepted() {
        assert!(parse_cron("0 5 * * 5").is_ok());
        assert!(parse_cron("*/15 * * * *").is_ok());
    }

    #[test]
    fn a_typo_is_rejected_rather_than_accepted_and_silently_never_fired() {
        assert!(parse_cron("not a schedule").is_err());
        assert!(parse_cron("99 * * * *").is_err());
    }

    #[test]
    fn blank_form_fields_become_absent_rather_than_empty_strings() {
        assert_eq!(blank_to_none("  "), None);
        assert_eq!(blank_to_none(" main "), Some("main".to_string()));
    }
}
