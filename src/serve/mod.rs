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

mod pages;

use crate::forge::Forge;
use crate::notify::{self, Notifier};
use crate::runner::{self, ForgeSpec, ForkSpec, Options};
use crate::store::{NewFork, Settings, Store};
use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct App {
    store: Arc<Store>,
}

pub async fn serve(store: Store, addr: SocketAddr) -> Result<()> {
    let app = App {
        store: Arc::new(store),
    };

    let scheduler = app.clone();
    tokio::spawn(async move { schedule_loop(scheduler).await });

    let router = Router::new()
        .route("/", get(pages::dashboard))
        .route("/settings", get(pages::settings).post(save_settings))
        .route("/settings/forge-token", post(save_forge_token))
        .route("/settings/telegram-token", post(save_telegram_token))
        .route("/forks/new", get(pages::new_fork))
        .route("/forks", post(create_fork))
        .route("/forks/{id}", get(pages::edit_fork).post(update_fork))
        .route("/forks/{id}/delete", post(delete_fork))
        .route("/run", post(trigger_run))
        .route("/runs/{id}", get(pages::run_detail))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    println!("weir: listening on http://{addr}");
    if !addr.ip().is_loopback() {
        println!(
            "weir: WARNING — bound to {}, which is not loopback. Anything that can reach \
             this can change which repositories get force-pushed.",
            addr.ip()
        );
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

    /// Everything a sync needs, assembled from the database.
    fn forge_spec(&self) -> Result<ForgeSpec> {
        let settings = self.store.settings()?;
        Ok(ForgeSpec {
            url: settings.forge_url,
            owner: settings.forge_owner,
            username: settings.forge_username,
            token: self.store.forge_token()?,
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
    fn sync_one_blocking(&self, fork: crate::store::Fork, dry_run: bool) {
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
            .forge_spec()
            .and_then(|forge| Ok((forge, self.options(dry_run)?)))
            .and_then(|(forge, options)| runner::sync_fork(&forge, &spec, &options));

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

#[derive(Deserialize)]
pub struct SettingsForm {
    forge_url: String,
    forge_owner: String,
    forge_username: String,
    sync_branch: String,
    boundary_file: String,
    schedule: String,
    telegram_chat: String,
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
        forge_url: form.forge_url.trim().to_string(),
        forge_owner: form.forge_owner.trim().to_string(),
        forge_username: blank_to_none(&form.forge_username),
        sync_branch: form.sync_branch.trim().to_string(),
        boundary_file: form.boundary_file.trim().to_string(),
        schedule,
        telegram_chat: blank_to_none(&form.telegram_chat),
    };
    match app.store.save_settings(&settings) {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(error) => pages::error_page("Could not save settings", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct TokenForm {
    token: String,
}

async fn save_forge_token(
    State(app): State<App>,
    axum::Form(form): axum::Form<TokenForm>,
) -> impl IntoResponse {
    match app.store.set_forge_token(form.token.trim()) {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(error) => pages::error_page("Could not store the token", &format!("{error:#}")),
    }
}

async fn save_telegram_token(
    State(app): State<App>,
    axum::Form(form): axum::Form<TokenForm>,
) -> impl IntoResponse {
    match app.store.set_telegram_token(form.token.trim()) {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(error) => pages::error_page("Could not store the token", &format!("{error:#}")),
    }
}

#[derive(Deserialize)]
pub struct ForkForm {
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
    let forks = match app.store.forks() {
        Ok(forks) => forks,
        Err(error) => return pages::error_page("Could not read the forks", &format!("{error:#}")),
    };
    let selected: Vec<_> = forks
        .into_iter()
        .filter(|f| f.enabled)
        .filter(|f| request.repo.as_deref().is_none_or(|r| r == f.repo))
        .collect();

    // Handed to a blocking thread and not waited on: a sync takes minutes, and
    // a browser request that hangs that long is one the user will reload,
    // starting a second sync on top of the first.
    let worker = app.clone();
    tokio::task::spawn_blocking(move || {
        for fork in selected {
            worker.sync_one_blocking(fork, dry_run);
        }
    });

    Redirect::to("/").into_response()
}

#[derive(Deserialize)]
pub struct DiscoverQuery {
    #[serde(default)]
    pub discover: Option<String>,
}

/// Repositories on the forge that are not already configured.
pub fn discover(app: &App) -> Result<Vec<crate::forge::Discovered>> {
    let settings = app.store.settings()?;
    let token = app
        .store
        .forge_token()?
        .context("no forge token is set, so the forge cannot be asked what it has")?;
    let gitea =
        crate::forge::gitea::Gitea::new(&settings.forge_url, &settings.forge_owner, &token)?;
    let known: Vec<String> = app.store.forks()?.into_iter().map(|f| f.repo).collect();
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

        let Ok(forks) = app.store.forks() else {
            continue;
        };
        let worker = app.clone();
        tokio::task::spawn_blocking(move || {
            for fork in forks.into_iter().filter(|f| f.enabled) {
                worker.sync_one_blocking(fork, false);
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
