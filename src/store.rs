//! Settings, forks, run history, and an audit trail, in SQLite.
//!
//! This is what the web UI edits, and it is the source of truth only for
//! `weir serve`. The one-shot `weir run --config` path reads a TOML file and
//! never opens this database — the two are never merged, so there is always
//! exactly one answer to "where did that setting come from".
//!
//! # What is deliberately *not* in here
//!
//! **The sync boundary.** It stays a file in the repository, as
//! [`crate::boundary`] explains. A row written when a sync *starts* would claim
//! the fork had advanced even if nobody merged the pull request, and closing a
//! pull request unmerged has to keep costing nothing.
//!
//! **Anything you would mind losing.** Delete this database and you lose your
//! settings and your history — not correctness. The next run reads the boundary
//! out of the repositories and carries on.
//!
//! # The token
//!
//! The forge token lives here, which makes the database file a secret. It is
//! created 0600, never rendered back to the browser, and never written to the
//! audit trail. That is a real trade against keeping it in the environment, and
//! it is made deliberately: a UI that cannot set credentials is a UI you still
//! have to edit files to use.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub forge_url: String,
    pub forge_owner: String,
    pub forge_username: Option<String>,
    pub sync_branch: String,
    pub boundary_file: String,
    /// A cron expression, or `None` when nothing is scheduled.
    pub schedule: Option<String>,
    pub telegram_chat: Option<String>,
}

/// Whether a secret is present, without carrying it around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretStatus {
    pub forge_token: bool,
    pub telegram_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fork {
    pub id: i64,
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFork {
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
    pub enabled: bool,
}

/// One entry in the audit trail: when, what changed, and both values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub at: String,
    pub what: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub repo: String,
    pub dry_run: bool,
    pub outcome: Option<String>,
    pub detail: String,
    pub pr_url: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening the database at {}", path.display()))?;
        restrict(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS settings (
                id             INTEGER PRIMARY KEY CHECK (id = 1),
                forge_url      TEXT NOT NULL DEFAULT '',
                forge_owner    TEXT NOT NULL DEFAULT '',
                forge_username TEXT,
                forge_token    TEXT,
                sync_branch    TEXT NOT NULL DEFAULT 'upstream-sync',
                boundary_file  TEXT NOT NULL DEFAULT '.upstream-sync',
                schedule       TEXT,
                telegram_token TEXT,
                telegram_chat  TEXT
            );
            INSERT OR IGNORE INTO settings (id) VALUES (1);

            CREATE TABLE IF NOT EXISTS forks (
                id              INTEGER PRIMARY KEY,
                repo            TEXT NOT NULL UNIQUE,
                upstream        TEXT NOT NULL,
                branch          TEXT NOT NULL,
                upstream_branch TEXT,
                keep_removed    TEXT NOT NULL DEFAULT '',
                enabled         INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS runs (
                id          INTEGER PRIMARY KEY,
                started_at  TEXT NOT NULL,
                finished_at TEXT,
                repo        TEXT NOT NULL,
                dry_run     INTEGER NOT NULL DEFAULT 0,
                outcome     TEXT,
                detail      TEXT NOT NULL DEFAULT '',
                pr_url      TEXT
            );
            CREATE INDEX IF NOT EXISTS runs_started ON runs (started_at DESC);

            -- What a form changed, and when. A database behind a UI loses the
            -- history a git-tracked config file gave for free; this buys most
            -- of it back for the price of one table.
            CREATE TABLE IF NOT EXISTS audit (
                id        INTEGER PRIMARY KEY,
                at        TEXT NOT NULL,
                what      TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT
            );
            CREATE INDEX IF NOT EXISTS audit_at ON audit (at DESC);
            "#,
        )
        .context("creating the schema")?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let settings = conn.query_row(
            "SELECT forge_url, forge_owner, forge_username, sync_branch, boundary_file,
                    schedule, telegram_chat
             FROM settings WHERE id = 1",
            [],
            |row| {
                Ok(Settings {
                    forge_url: row.get(0)?,
                    forge_owner: row.get(1)?,
                    forge_username: row.get(2)?,
                    sync_branch: row.get(3)?,
                    boundary_file: row.get(4)?,
                    schedule: row.get(5)?,
                    telegram_chat: row.get(6)?,
                })
            },
        )?;
        Ok(settings)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let before = self.settings()?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE settings SET forge_url = ?1, forge_owner = ?2, forge_username = ?3,
                        sync_branch = ?4, boundary_file = ?5, schedule = ?6, telegram_chat = ?7
                 WHERE id = 1",
                params![
                    settings.forge_url,
                    settings.forge_owner,
                    settings.forge_username,
                    settings.sync_branch,
                    settings.boundary_file,
                    settings.schedule,
                    settings.telegram_chat,
                ],
            )?;
        }
        // Recorded field by field so the trail says what moved, not merely that
        // somebody pressed save.
        for (what, old, new) in [
            ("forge url", &before.forge_url, &settings.forge_url),
            ("forge owner", &before.forge_owner, &settings.forge_owner),
            ("sync branch", &before.sync_branch, &settings.sync_branch),
            (
                "boundary file",
                &before.boundary_file,
                &settings.boundary_file,
            ),
        ] {
            if old != new {
                self.record(what, Some(old), Some(new))?;
            }
        }
        if before.schedule != settings.schedule {
            self.record(
                "schedule",
                before.schedule.as_deref(),
                settings.schedule.as_deref(),
            )?;
        }
        Ok(())
    }

    /// Stores the forge token. Never echoed anywhere; the audit trail records
    /// only that it changed.
    pub fn set_forge_token(&self, token: &str) -> Result<()> {
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE settings SET forge_token = ?1 WHERE id = 1",
                params![token],
            )?;
        }
        self.record("forge token", None, Some("(replaced)"))
    }

    pub fn set_telegram_token(&self, token: &str) -> Result<()> {
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE settings SET telegram_token = ?1 WHERE id = 1",
                params![token],
            )?;
        }
        self.record("telegram token", None, Some("(replaced)"))
    }

    pub fn forge_token(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        Ok(conn
            .query_row("SELECT forge_token FROM settings WHERE id = 1", [], |row| {
                row.get::<_, Option<String>>(0)
            })?
            .filter(|t| !t.trim().is_empty()))
    }

    pub fn telegram_token(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        Ok(conn
            .query_row(
                "SELECT telegram_token FROM settings WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )?
            .filter(|t| !t.trim().is_empty()))
    }

    /// Whether each secret is present, for a UI that must not show them.
    pub fn secret_status(&self) -> Result<SecretStatus> {
        Ok(SecretStatus {
            forge_token: self.forge_token()?.is_some(),
            telegram_token: self.telegram_token()?.is_some(),
        })
    }

    pub fn forks(&self) -> Result<Vec<Fork>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, repo, upstream, branch, upstream_branch, keep_removed, enabled
             FROM forks ORDER BY repo",
        )?;
        let forks = statement
            .query_map([], |row| {
                Ok(Fork {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    upstream: row.get(2)?,
                    branch: row.get(3)?,
                    upstream_branch: row.get(4)?,
                    keep_removed: split_paths(&row.get::<_, String>(5)?),
                    enabled: row.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(forks)
    }

    pub fn fork(&self, id: i64) -> Result<Option<Fork>> {
        Ok(self.forks()?.into_iter().find(|f| f.id == id))
    }

    pub fn add_fork(&self, fork: &NewFork) -> Result<i64> {
        let id = {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "INSERT INTO forks (repo, upstream, branch, upstream_branch, keep_removed, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    fork.repo,
                    fork.upstream,
                    fork.branch,
                    fork.upstream_branch,
                    fork.keep_removed.join("\n"),
                    fork.enabled as i64,
                ],
            )
            .with_context(|| format!("adding fork {:?}", fork.repo))?;
            conn.last_insert_rowid()
        };
        self.record("fork added", None, Some(&fork.repo))?;
        Ok(id)
    }

    pub fn update_fork(&self, id: i64, fork: &NewFork) -> Result<()> {
        let before = self.fork(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE forks SET repo = ?1, upstream = ?2, branch = ?3, upstream_branch = ?4,
                        keep_removed = ?5, enabled = ?6
                 WHERE id = ?7",
                params![
                    fork.repo,
                    fork.upstream,
                    fork.branch,
                    fork.upstream_branch,
                    fork.keep_removed.join("\n"),
                    fork.enabled as i64,
                    id,
                ],
            )?;
        }
        // The target branch is the one worth shouting about: change it and syncs
        // start landing somewhere else entirely.
        if let Some(before) = before {
            if before.branch != fork.branch {
                self.record(
                    &format!("{} target branch", fork.repo),
                    Some(&before.branch),
                    Some(&fork.branch),
                )?;
            }
            if before.enabled != fork.enabled {
                self.record(
                    &format!("{} enabled", fork.repo),
                    Some(&before.enabled.to_string()),
                    Some(&fork.enabled.to_string()),
                )?;
            }
        }
        Ok(())
    }

    pub fn delete_fork(&self, id: i64) -> Result<()> {
        let before = self.fork(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute("DELETE FROM forks WHERE id = ?1", params![id])?;
        }
        if let Some(before) = before {
            self.record("fork removed", Some(&before.repo), None)?;
        }
        Ok(())
    }

    pub fn start_run(&self, repo: &str, dry_run: bool) -> Result<i64> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        conn.execute(
            "INSERT INTO runs (started_at, repo, dry_run) VALUES (?1, ?2, ?3)",
            params![now(), repo, dry_run as i64],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_run(
        &self,
        id: i64,
        outcome: &str,
        detail: &str,
        pr_url: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        conn.execute(
            "UPDATE runs SET finished_at = ?1, outcome = ?2, detail = ?3, pr_url = ?4
             WHERE id = ?5",
            params![now(), outcome, detail, pr_url, id],
        )?;
        Ok(())
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<Run>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, started_at, finished_at, repo, dry_run, outcome, detail, pr_url
             FROM runs ORDER BY id DESC LIMIT ?1",
        )?;
        let runs = statement
            .query_map(params![limit as i64], |row| {
                Ok(Run {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    finished_at: row.get(2)?,
                    repo: row.get(3)?,
                    dry_run: row.get::<_, i64>(4)? != 0,
                    outcome: row.get(5)?,
                    detail: row.get(6)?,
                    pr_url: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    pub fn run(&self, id: i64) -> Result<Option<Run>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let run = conn
            .query_row(
                "SELECT id, started_at, finished_at, repo, dry_run, outcome, detail, pr_url
                 FROM runs WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Run {
                        id: row.get(0)?,
                        started_at: row.get(1)?,
                        finished_at: row.get(2)?,
                        repo: row.get(3)?,
                        dry_run: row.get::<_, i64>(4)? != 0,
                        outcome: row.get(5)?,
                        detail: row.get(6)?,
                        pr_url: row.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(run)
    }

    pub fn record(&self, what: &str, old: Option<&str>, new: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        conn.execute(
            "INSERT INTO audit (at, what, old_value, new_value) VALUES (?1, ?2, ?3, ?4)",
            params![now(), what, old, new],
        )?;
        Ok(())
    }

    pub fn audit(&self, limit: usize) -> Result<Vec<Change>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT at, what, old_value, new_value FROM audit ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok(Change {
                    at: row.get(0)?,
                    what: row.get(1)?,
                    old_value: row.get(2)?,
                    new_value: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn split_paths(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The database holds the forge token, so nobody else on the host may read it.
fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork(repo: &str) -> NewFork {
        NewFork {
            repo: repo.to_string(),
            upstream: format!("https://example.invalid/{repo}.git"),
            branch: "main".to_string(),
            upstream_branch: None,
            keep_removed: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn a_fresh_database_has_usable_defaults() {
        let store = Store::in_memory().unwrap();
        let settings = store.settings().unwrap();
        assert_eq!(settings.sync_branch, "upstream-sync");
        assert_eq!(settings.boundary_file, ".upstream-sync");
        assert_eq!(settings.schedule, None, "nothing is scheduled until asked");
    }

    #[test]
    fn migrating_twice_is_harmless() {
        let store = Store::in_memory().unwrap();
        store.add_fork(&fork("codex")).unwrap();
        store.migrate().unwrap();
        assert_eq!(store.forks().unwrap().len(), 1, "data survives");
    }

    #[test]
    fn forks_round_trip() {
        let store = Store::in_memory().unwrap();
        let mut new = fork("codex");
        new.keep_removed = vec![".github/workflows/a.yml".into(), "b.yml".into()];
        new.upstream_branch = Some("master".into());
        let id = store.add_fork(&new).unwrap();

        let stored = store.fork(id).unwrap().expect("just added");
        assert_eq!(stored.repo, "codex");
        assert_eq!(stored.keep_removed, new.keep_removed);
        assert_eq!(stored.upstream_branch.as_deref(), Some("master"));
        assert!(stored.enabled);
    }

    #[test]
    fn the_same_fork_cannot_be_added_twice() {
        let store = Store::in_memory().unwrap();
        store.add_fork(&fork("codex")).unwrap();
        assert!(store.add_fork(&fork("codex")).is_err());
    }

    #[test]
    fn forks_may_disagree_about_their_base_branch() {
        let store = Store::in_memory().unwrap();
        store.add_fork(&fork("codex")).unwrap();
        let mut dokploy = fork("dokploy");
        dokploy.branch = "canary".into();
        store.add_fork(&dokploy).unwrap();

        let branches: Vec<_> = store
            .forks()
            .unwrap()
            .into_iter()
            .map(|f| f.branch)
            .collect();
        assert_eq!(branches, vec!["main", "canary"]);
    }

    /// The token is the reason the database file is sensitive. It must never
    /// come back out through anything a page can render.
    #[test]
    fn the_token_is_readable_only_deliberately_and_never_audited() {
        let store = Store::in_memory().unwrap();
        assert!(!store.secret_status().unwrap().forge_token);

        store.set_forge_token("super-secret-value").unwrap();

        assert!(store.secret_status().unwrap().forge_token);
        assert_eq!(
            store.forge_token().unwrap().as_deref(),
            Some("super-secret-value")
        );

        let trail = format!("{:?}", store.audit(10).unwrap());
        assert!(!trail.contains("super-secret-value"), "{trail}");
        assert!(trail.contains("forge token"), "but the change is recorded");
    }

    #[test]
    fn an_empty_token_reads_as_absent_rather_than_as_a_blank_credential() {
        let store = Store::in_memory().unwrap();
        store.set_forge_token("   ").unwrap();
        assert_eq!(store.forge_token().unwrap(), None);
        assert!(!store.secret_status().unwrap().forge_token);
    }

    /// The whole reason the audit table exists: a form loses the history a
    /// git-tracked file gave for free.
    #[test]
    fn changing_a_target_branch_is_recorded_with_both_values() {
        let store = Store::in_memory().unwrap();
        let id = store.add_fork(&fork("dokploy")).unwrap();

        let mut changed = fork("dokploy");
        changed.branch = "canary".into();
        store.update_fork(id, &changed).unwrap();

        let trail = store.audit(10).unwrap();
        let entry = trail
            .iter()
            .find(|c| c.what == "dokploy target branch")
            .expect("the change is recorded");
        assert_eq!(entry.old_value.as_deref(), Some("main"));
        assert_eq!(entry.new_value.as_deref(), Some("canary"));
    }

    #[test]
    fn saving_settings_unchanged_records_nothing() {
        let store = Store::in_memory().unwrap();
        let settings = store.settings().unwrap();
        store.save_settings(&settings).unwrap();
        assert!(store.audit(10).unwrap().is_empty());
    }

    #[test]
    fn a_schedule_change_is_recorded() {
        let store = Store::in_memory().unwrap();
        let mut settings = store.settings().unwrap();
        settings.schedule = Some("0 5 * * 5".into());
        store.save_settings(&settings).unwrap();

        let trail = store.audit(10).unwrap();
        assert!(trail
            .iter()
            .any(|c| c.what == "schedule" && c.new_value.as_deref() == Some("0 5 * * 5")));
    }

    #[test]
    fn runs_are_recorded_from_start_to_finish() {
        let store = Store::in_memory().unwrap();
        let id = store.start_run("codex", true).unwrap();

        let open = store.run(id).unwrap().unwrap();
        assert!(open.finished_at.is_none(), "still running");
        assert!(open.dry_run);

        store
            .finish_run(
                id,
                "conflicts",
                "3 conflicting paths",
                Some("https://x/pulls/1"),
            )
            .unwrap();

        let done = store.run(id).unwrap().unwrap();
        assert_eq!(done.outcome.as_deref(), Some("conflicts"));
        assert_eq!(done.pr_url.as_deref(), Some("https://x/pulls/1"));
        assert!(done.finished_at.is_some());
    }

    #[test]
    fn recent_runs_come_back_newest_first() {
        let store = Store::in_memory().unwrap();
        store.start_run("codex", false).unwrap();
        store.start_run("dokploy", false).unwrap();

        let runs = store.recent_runs(10).unwrap();
        assert_eq!(runs[0].repo, "dokploy");
        assert_eq!(runs[1].repo, "codex");
    }

    #[test]
    fn deleting_a_fork_leaves_a_record_of_what_went() {
        let store = Store::in_memory().unwrap();
        let id = store.add_fork(&fork("codex")).unwrap();
        store.delete_fork(id).unwrap();

        assert!(store.forks().unwrap().is_empty());
        let trail = store.audit(10).unwrap();
        assert!(trail
            .iter()
            .any(|c| c.what == "fork removed" && c.old_value.as_deref() == Some("codex")));
    }

    #[test]
    fn the_database_file_is_not_world_readable() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("weir.db");
        let store = Store::open(&path).unwrap();
        store.set_forge_token("secret").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "mode is {:o}", mode & 0o777);
        }
    }
}
