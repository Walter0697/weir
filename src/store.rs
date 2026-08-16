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
use rusqlite::{params, Connection as Sqlite, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Sqlite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub sync_branch: String,
    pub boundary_file: String,
    /// A cron expression, or `None` when nothing is scheduled.
    pub schedule: Option<String>,
    pub telegram_chat: Option<String>,
}

/// A forge and the credential for it, which are one thing rather than two: the
/// URL can do nothing useful for a private repository without the token, and
/// the token means nothing without the URL it belongs to.
///
/// A list rather than a singleton, so a second instance — or the same instance
/// under a different account — is another row rather than another deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub url: String,
    pub username: Option<String>,
    /// Reported, never returned. Use [`Store::connection_token`] deliberately.
    pub has_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConnection {
    pub name: String,
    pub kind: String,
    pub url: String,
    pub username: Option<String>,
}

/// Whether a secret is present, without carrying it around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretStatus {
    pub telegram_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fork {
    pub id: i64,
    pub connection_id: i64,
    /// The user or organisation the fork lives under. On the fork rather than
    /// the connection, so one forge can hold repositories under several owners.
    pub owner: String,
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFork {
    pub connection_id: i64,
    pub owner: String,
    pub repo: String,
    pub upstream: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub keep_removed: Vec<String>,
    pub enabled: bool,
}

/// One entry in the audit trail: when, what changed, and both values.
/// A rule covering every repository under one owner, expanded fresh each run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub id: i64,
    pub connection_id: i64,
    pub owner: String,
    /// Names or `*` patterns that this watch does not cover.
    pub except: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWatch {
    pub connection_id: i64,
    pub owner: String,
    pub except: Vec<String>,
    pub enabled: bool,
}

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
        let conn = Sqlite::open(path)
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
            conn: Mutex::new(Sqlite::open_in_memory()?),
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

            -- A forge and its credential, together, because neither is useful
            -- without the other. Several rows so a second instance, or the same
            -- one under a different account, is a row rather than a deployment.
            CREATE TABLE IF NOT EXISTS connections (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                kind     TEXT NOT NULL DEFAULT 'gitea',
                url      TEXT NOT NULL,
                username TEXT,
                token    TEXT
            );

            CREATE TABLE IF NOT EXISTS forks (
                id              INTEGER PRIMARY KEY,
                repo            TEXT NOT NULL UNIQUE,
                upstream        TEXT NOT NULL,
                branch          TEXT NOT NULL,
                upstream_branch TEXT,
                keep_removed    TEXT NOT NULL DEFAULT '',
                enabled         INTEGER NOT NULL DEFAULT 1
            );

            -- Watching an owner rather than listing its repositories. Expanded
            -- at run time, so a repository added to the forge later is covered
            -- without anyone editing configuration.
            CREATE TABLE IF NOT EXISTS watches (
                id            INTEGER PRIMARY KEY,
                connection_id INTEGER NOT NULL,
                owner         TEXT NOT NULL,
                except_list   TEXT NOT NULL DEFAULT '',
                enabled       INTEGER NOT NULL DEFAULT 1,
                UNIQUE (connection_id, owner)
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

        // `owner` belongs to the fork rather than the forge: one instance can
        // hold repositories under several organisations, and the earlier schema
        // could not express that.
        let fork_columns: Vec<String> = {
            let mut statement = conn.prepare("PRAGMA table_info(forks)")?;
            let names = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names
        };
        if !fork_columns.iter().any(|c| c == "connection_id") {
            conn.execute(
                "ALTER TABLE forks ADD COLUMN connection_id INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !fork_columns.iter().any(|c| c == "owner") {
            conn.execute(
                "ALTER TABLE forks ADD COLUMN owner TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }

        // Carry an existing single-forge install across rather than making
        // somebody retype what they already entered.
        let connections: i64 =
            conn.query_row("SELECT count(*) FROM connections", [], |row| row.get(0))?;
        if connections == 0 {
            let legacy: Option<(String, String, Option<String>, Option<String>)> = conn
                .query_row(
                    "SELECT forge_url, forge_owner, forge_username, forge_token
                     FROM settings WHERE id = 1 AND forge_url <> ''",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if let Some((url, owner, username, token)) = legacy {
                conn.execute(
                    "INSERT INTO connections (name, kind, url, username, token)
                     VALUES (?1, 'gitea', ?2, ?3, ?4)",
                    params!["default", url, username, token],
                )?;
                let id = conn.last_insert_rowid();
                conn.execute(
                    "UPDATE forks SET connection_id = ?1, owner = ?2
                     WHERE connection_id = 0 OR owner = ''",
                    params![id, owner],
                )?;
            }
        }
        Ok(())
    }

    pub fn connections(&self) -> Result<Vec<Connection>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, name, kind, url, username,
                    CASE WHEN token IS NULL OR trim(token) = '' THEN 0 ELSE 1 END
             FROM connections ORDER BY name",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Connection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    url: row.get(3)?,
                    username: row.get(4)?,
                    has_token: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn connection(&self, id: i64) -> Result<Option<Connection>> {
        Ok(self.connections()?.into_iter().find(|c| c.id == id))
    }

    pub fn add_connection(&self, new: &NewConnection, token: &str) -> Result<i64> {
        let id = {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "INSERT INTO connections (name, kind, url, username, token)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![new.name, new.kind, new.url, new.username, token],
            )
            .with_context(|| format!("adding connection {:?}", new.name))?;
            conn.last_insert_rowid()
        };
        self.record("connection added", None, Some(&new.name))?;
        Ok(id)
    }

    /// A blank token leaves the stored one alone: it is never rendered back, so
    /// it cannot be round-tripped through a form.
    pub fn update_connection(&self, id: i64, new: &NewConnection, token: &str) -> Result<()> {
        let before = self.connection(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE connections SET name = ?1, kind = ?2, url = ?3, username = ?4 WHERE id = ?5",
                params![new.name, new.kind, new.url, new.username, id],
            )?;
            if !token.trim().is_empty() {
                conn.execute(
                    "UPDATE connections SET token = ?1 WHERE id = ?2",
                    params![token.trim(), id],
                )?;
            }
        }
        if let Some(before) = before {
            if before.url != new.url {
                self.record(
                    &format!("{} url", new.name),
                    Some(&before.url),
                    Some(&new.url),
                )?;
            }
        }
        if !token.trim().is_empty() {
            self.record(&format!("{} token", new.name), None, Some("(replaced)"))?;
        }
        Ok(())
    }

    pub fn delete_connection(&self, id: i64) -> Result<()> {
        let before = self.connection(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute("DELETE FROM connections WHERE id = ?1", params![id])?;
        }
        if let Some(before) = before {
            self.record("connection removed", Some(&before.name), None)?;
        }
        Ok(())
    }

    /// Deliberately separate from [`Store::connections`], so a page cannot
    /// render a token by accident.
    pub fn connection_token(&self, id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        Ok(conn
            .query_row(
                "SELECT token FROM connections WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()))
    }

    /// How many forks reference a connection, so removing one can say what it
    /// would strand.
    pub fn forks_using(&self, connection_id: i64) -> Result<usize> {
        Ok(self
            .forks()?
            .into_iter()
            .filter(|f| f.connection_id == connection_id)
            .count())
    }

    pub fn settings(&self) -> Result<Settings> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let settings = conn.query_row(
            "SELECT sync_branch, boundary_file, schedule, telegram_chat
             FROM settings WHERE id = 1",
            [],
            |row| {
                Ok(Settings {
                    sync_branch: row.get(0)?,
                    boundary_file: row.get(1)?,
                    schedule: row.get(2)?,
                    telegram_chat: row.get(3)?,
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
                "UPDATE settings SET sync_branch = ?1, boundary_file = ?2, schedule = ?3,
                        telegram_chat = ?4
                 WHERE id = 1",
                params![
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
            telegram_token: self.telegram_token()?.is_some(),
        })
    }

    pub fn forks(&self) -> Result<Vec<Fork>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, connection_id, owner, repo, upstream, branch, upstream_branch,
                    keep_removed, enabled
             FROM forks ORDER BY owner, repo",
        )?;
        let forks = statement
            .query_map([], |row| {
                Ok(Fork {
                    id: row.get(0)?,
                    connection_id: row.get(1)?,
                    owner: row.get(2)?,
                    repo: row.get(3)?,
                    upstream: row.get(4)?,
                    branch: row.get(5)?,
                    upstream_branch: row.get(6)?,
                    keep_removed: split_paths(&row.get::<_, String>(7)?),
                    enabled: row.get::<_, i64>(8)? != 0,
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
                "INSERT INTO forks (connection_id, owner, repo, upstream, branch,
                        upstream_branch, keep_removed, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    fork.connection_id,
                    fork.owner,
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
                "UPDATE forks SET connection_id = ?1, owner = ?2, repo = ?3, upstream = ?4,
                        branch = ?5, upstream_branch = ?6, keep_removed = ?7, enabled = ?8
                 WHERE id = ?9",
                params![
                    fork.connection_id,
                    fork.owner,
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

    pub fn watches(&self) -> Result<Vec<Watch>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, connection_id, owner, except_list, enabled FROM watches ORDER BY owner",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Watch {
                    id: row.get(0)?,
                    connection_id: row.get(1)?,
                    owner: row.get(2)?,
                    except: split_paths(&row.get::<_, String>(3)?),
                    enabled: row.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn watch(&self, id: i64) -> Result<Option<Watch>> {
        Ok(self.watches()?.into_iter().find(|w| w.id == id))
    }

    pub fn add_watch(&self, watch: &NewWatch) -> Result<i64> {
        let id = {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "INSERT INTO watches (connection_id, owner, except_list, enabled)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    watch.connection_id,
                    watch.owner,
                    watch.except.join("\n"),
                    watch.enabled as i64,
                ],
            )
            .with_context(|| format!("watching {:?}", watch.owner))?;
            conn.last_insert_rowid()
        };
        self.record("watch added", None, Some(&watch.owner))?;
        Ok(id)
    }

    pub fn update_watch(&self, id: i64, watch: &NewWatch) -> Result<()> {
        let before = self.watch(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute(
                "UPDATE watches SET connection_id = ?1, owner = ?2, except_list = ?3, enabled = ?4
                 WHERE id = ?5",
                params![
                    watch.connection_id,
                    watch.owner,
                    watch.except.join("\n"),
                    watch.enabled as i64,
                    id,
                ],
            )?;
        }
        // The exception list decides what a watch does *not* touch, so a change
        // to it is worth a line in the trail.
        if let Some(before) = before {
            if before.except != watch.except {
                self.record(
                    &format!("{} exceptions", watch.owner),
                    Some(&before.except.join(", ")),
                    Some(&watch.except.join(", ")),
                )?;
            }
            if before.enabled != watch.enabled {
                self.record(
                    &format!("{} watch enabled", watch.owner),
                    Some(&before.enabled.to_string()),
                    Some(&watch.enabled.to_string()),
                )?;
            }
        }
        Ok(())
    }

    pub fn delete_watch(&self, id: i64) -> Result<()> {
        let before = self.watch(id)?;
        {
            let conn = self.conn.lock().expect("the store lock is never poisoned");
            conn.execute("DELETE FROM watches WHERE id = ?1", params![id])?;
        }
        if let Some(before) = before {
            self.record("watch removed", Some(&before.owner), None)?;
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

    /// Recent runs for one repository, so its own page can show its history
    /// rather than making somebody scan the whole list for the name.
    pub fn runs_for(&self, repo: &str, limit: usize) -> Result<Vec<Run>> {
        let conn = self.conn.lock().expect("the store lock is never poisoned");
        let mut statement = conn.prepare(
            "SELECT id, started_at, finished_at, repo, dry_run, outcome, detail, pr_url
             FROM runs WHERE repo = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let runs = statement
            .query_map(params![repo, limit as i64], |row| {
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

    fn store_with_connection() -> (Store, i64) {
        let store = Store::in_memory().unwrap();
        let id = store
            .add_connection(
                &NewConnection {
                    name: "home".into(),
                    kind: "gitea".into(),
                    url: "https://forge.example".into(),
                    username: Some("weir-bot".into()),
                },
                "secret-token-value",
            )
            .unwrap();
        (store, id)
    }

    fn fork(connection_id: i64, owner: &str, repo: &str) -> NewFork {
        NewFork {
            connection_id,
            owner: owner.to_string(),
            repo: repo.to_string(),
            upstream: format!("https://example.invalid/{repo}.git"),
            branch: "main".to_string(),
            upstream_branch: None,
            keep_removed: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn a_fresh_database_has_usable_defaults_and_no_connections() {
        let store = Store::in_memory().unwrap();
        let settings = store.settings().unwrap();
        assert_eq!(settings.sync_branch, "upstream-sync");
        assert_eq!(settings.boundary_file, ".upstream-sync");
        assert_eq!(settings.schedule, None, "nothing is scheduled until asked");
        assert!(store.connections().unwrap().is_empty());
    }

    #[test]
    fn migrating_twice_is_harmless() {
        let (store, id) = store_with_connection();
        store.add_fork(&fork(id, "org", "codex")).unwrap();
        store.migrate().unwrap();
        assert_eq!(store.forks().unwrap().len(), 1, "data survives");
        assert_eq!(store.connections().unwrap().len(), 1);
    }

    /// A forge and its credential are one row, and the credential never comes
    /// back out through the listing a page renders.
    #[test]
    fn a_connection_reports_that_it_has_a_token_without_handing_it_over() {
        let (store, id) = store_with_connection();

        let listed = &store.connections().unwrap()[0];
        assert_eq!(listed.name, "home");
        assert_eq!(listed.url, "https://forge.example");
        assert!(listed.has_token);
        let rendered = format!("{listed:?}");
        assert!(!rendered.contains("secret-token-value"), "{rendered}");

        assert_eq!(
            store.connection_token(id).unwrap().as_deref(),
            Some("secret-token-value"),
            "but it is there when asked for deliberately"
        );
    }

    #[test]
    fn a_blank_token_on_update_keeps_the_stored_one() {
        let (store, id) = store_with_connection();
        let renamed = NewConnection {
            name: "home gitea".into(),
            kind: "gitea".into(),
            url: "https://forge.example".into(),
            username: None,
        };
        store.update_connection(id, &renamed, "   ").unwrap();

        assert_eq!(
            store.connection_token(id).unwrap().as_deref(),
            Some("secret-token-value"),
            "editing the name must not require pasting the token again"
        );
        assert_eq!(store.connections().unwrap()[0].name, "home gitea");
    }

    #[test]
    fn a_replaced_token_is_recorded_without_either_value() {
        let (store, id) = store_with_connection();
        store
            .update_connection(
                id,
                &NewConnection {
                    name: "home".into(),
                    kind: "gitea".into(),
                    url: "https://forge.example".into(),
                    username: None,
                },
                "a-different-secret",
            )
            .unwrap();

        let trail = format!("{:?}", store.audit(10).unwrap());
        assert!(!trail.contains("secret-token-value"), "{trail}");
        assert!(!trail.contains("a-different-secret"), "{trail}");
        assert!(trail.contains("home token"), "but the change is recorded");
    }

    #[test]
    fn an_empty_token_reads_as_absent_rather_than_as_a_blank_credential() {
        let store = Store::in_memory().unwrap();
        let id = store
            .add_connection(
                &NewConnection {
                    name: "empty".into(),
                    kind: "gitea".into(),
                    url: "https://forge.example".into(),
                    username: None,
                },
                "   ",
            )
            .unwrap();
        assert_eq!(store.connection_token(id).unwrap(), None);
        assert!(!store.connections().unwrap()[0].has_token);
    }

    /// The reason `owner` moved onto the fork: one forge, several
    /// organisations, which the old shape could not express at all.
    #[test]
    fn one_connection_can_hold_forks_under_several_owners() {
        let (store, id) = store_with_connection();
        store.add_fork(&fork(id, "opensource", "codex")).unwrap();
        store.add_fork(&fork(id, "internal", "runner")).unwrap();

        let owners: Vec<_> = store
            .forks()
            .unwrap()
            .into_iter()
            .map(|f| format!("{}/{}", f.owner, f.repo))
            .collect();
        assert_eq!(owners, vec!["internal/runner", "opensource/codex"]);
        assert_eq!(store.forks_using(id).unwrap(), 2);
    }

    #[test]
    fn forks_can_sit_on_different_forges() {
        let (store, home) = store_with_connection();
        let other = store
            .add_connection(
                &NewConnection {
                    name: "elsewhere".into(),
                    kind: "forgejo".into(),
                    url: "https://other.example".into(),
                    username: None,
                },
                "another-token",
            )
            .unwrap();
        store.add_fork(&fork(home, "org", "codex")).unwrap();
        store.add_fork(&fork(other, "org", "dokploy")).unwrap();

        assert_eq!(store.forks_using(home).unwrap(), 1);
        assert_eq!(store.forks_using(other).unwrap(), 1);
    }

    /// An install from before connections existed must not have to be retyped.
    #[test]
    fn an_older_single_forge_install_is_carried_across() {
        let store = Store::in_memory().unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE settings SET forge_url = 'https://old.example',
                        forge_owner = 'old-org', forge_username = 'bot', forge_token = 'old-token'
                 WHERE id = 1",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO forks (repo, upstream, branch, connection_id, owner)
                 VALUES ('codex', 'https://example.invalid/codex.git', 'main', 0, '')",
                [],
            )
            .unwrap();
        }

        store.migrate().unwrap();

        let connections = store.connections().unwrap();
        assert_eq!(connections.len(), 1, "the old forge became a connection");
        assert_eq!(connections[0].url, "https://old.example");
        assert!(connections[0].has_token);

        let forks = store.forks().unwrap();
        assert_eq!(forks[0].owner, "old-org", "the owner moved onto the fork");
        assert_eq!(forks[0].connection_id, connections[0].id);
    }

    #[test]
    fn forks_round_trip() {
        let (store, connection) = store_with_connection();
        let mut new = fork(connection, "org", "codex");
        new.keep_removed = vec![".github/workflows/a.yml".into(), "b.yml".into()];
        new.upstream_branch = Some("master".into());
        let id = store.add_fork(&new).unwrap();

        let stored = store.fork(id).unwrap().expect("just added");
        assert_eq!(stored.repo, "codex");
        assert_eq!(stored.owner, "org");
        assert_eq!(stored.connection_id, connection);
        assert_eq!(stored.keep_removed, new.keep_removed);
        assert_eq!(stored.upstream_branch.as_deref(), Some("master"));
        assert!(stored.enabled);
    }

    #[test]
    fn the_same_fork_cannot_be_added_twice() {
        let (store, id) = store_with_connection();
        store.add_fork(&fork(id, "org", "codex")).unwrap();
        assert!(store.add_fork(&fork(id, "org", "codex")).is_err());
    }

    #[test]
    fn forks_may_disagree_about_their_base_branch() {
        let (store, id) = store_with_connection();
        store.add_fork(&fork(id, "org", "codex")).unwrap();
        let mut dokploy = fork(id, "org", "dokploy");
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

    /// The whole reason the audit table exists: a form loses the history a
    /// git-tracked file gave for free.
    #[test]
    fn changing_a_target_branch_is_recorded_with_both_values() {
        let (store, connection) = store_with_connection();
        let id = store.add_fork(&fork(connection, "org", "dokploy")).unwrap();

        let mut changed = fork(connection, "org", "dokploy");
        changed.branch = "canary".into();
        store.update_fork(id, &changed).unwrap();

        let entry = store
            .audit(10)
            .unwrap()
            .into_iter()
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

        assert!(store
            .audit(10)
            .unwrap()
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
        let (store, connection) = store_with_connection();
        let id = store.add_fork(&fork(connection, "org", "codex")).unwrap();
        store.delete_fork(id).unwrap();

        assert!(store.forks().unwrap().is_empty());
        assert!(store
            .audit(10)
            .unwrap()
            .iter()
            .any(|c| c.what == "fork removed" && c.old_value.as_deref() == Some("codex")));
    }

    #[test]
    fn the_database_file_is_not_world_readable() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("weir.db");
        let store = Store::open(&path).unwrap();
        store
            .add_connection(
                &NewConnection {
                    name: "home".into(),
                    kind: "gitea".into(),
                    url: "https://forge.example".into(),
                    username: None,
                },
                "secret",
            )
            .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "mode is {:o}", mode & 0o777);
        }
    }
}
