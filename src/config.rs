//! The fork list and forge settings, read from a TOML file.
//!
//! This file is meant to be committed to a repository, so it holds no secrets —
//! only the *names* of environment variables that carry them. Everything the
//! tool needs to know about an installation lives here; nothing about any
//! particular org or host is compiled in.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

/// The only schema version this build understands.
///
/// It is checked before anything else so that a newer config fails with a clear
/// message instead of a confusing complaint about some individual field.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub forge: Forge,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(rename = "fork", default)]
    pub forks: Vec<Fork>,
    #[serde(rename = "notify", default)]
    pub notify: Vec<Notify>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forge {
    /// Which forge API to speak. Only `gitea` exists today; Forgejo answers the
    /// same API, so it is accepted as an alias rather than a separate impl.
    pub kind: ForgeKind,
    /// Base URL of the forge, e.g. `https://gitea.example.com`.
    pub url: String,
    /// The owner (user or org) the forks live under.
    pub owner: String,
    /// Name of the environment variable holding the forge token.
    #[serde(default = "default_token_env")]
    pub token_env: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Gitea,
    Forgejo,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Branch the sync is published to, and the head ref of the PR it opens.
    #[serde(default = "default_sync_branch")]
    pub sync_branch: String,
    /// Path, relative to the repository root, of the file recording which
    /// upstream commit the fork's content currently corresponds to.
    #[serde(default = "default_boundary_file")]
    pub boundary_file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fork {
    /// Repository name on the forge, without the owner.
    pub repo: String,
    /// Clone URL of the upstream project.
    pub upstream: String,
    /// Branch in our fork that the sync targets.
    pub branch: String,
    /// Branch to take from upstream. Defaults to `branch` — the forks that
    /// motivated this tool disagree about their base branch (`main` vs
    /// `canary`), so neither side may be assumed.
    #[serde(default)]
    pub upstream_branch: Option<String>,
    /// Paths this fork keeps removed, even when upstream edits or re-adds them.
    ///
    /// This is *not* "delete anything that conflicts". A modified fork
    /// conflicts often, and a conflict with no rule here is never resolved
    /// automatically — it gets an unmergeable pull request and a human. This
    /// list is the narrow exception: paths the fork deleted deliberately, where
    /// upstream keeps editing them, so git raises the same delete/modify
    /// conflict every sync and the answer is the same every time.
    ///
    /// Exact paths, never globs. A fork usually keeps its own files beside the
    /// ones it dropped — a fork that removed upstream's release workflows may
    /// well keep its own in the same directory — so a glob would quietly eat
    /// the wrong thing.
    ///
    /// This is the one place the sync exercises judgement, so it is declared
    /// per fork rather than decided in code, and every path it removes is
    /// named in the pull request body.
    #[serde(default)]
    pub keep_removed: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Notify {
    Telegram {
        #[serde(default = "default_telegram_token_env")]
        token_env: String,
        #[serde(default = "default_telegram_chat_env")]
        chat_env: String,
    },
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            sync_branch: default_sync_branch(),
            boundary_file: default_boundary_file(),
        }
    }
}

fn default_token_env() -> String {
    "WEIR_TOKEN".to_string()
}

fn default_sync_branch() -> String {
    "upstream-sync".to_string()
}

fn default_boundary_file() -> String {
    ".upstream-sync".to_string()
}

fn default_telegram_token_env() -> String {
    "TELEGRAM_BOT_TOKEN".to_string()
}

fn default_telegram_chat_env() -> String {
    "TELEGRAM_CHAT_ID".to_string()
}

impl Fork {
    /// The upstream branch to fetch, which defaults to the branch we target.
    pub fn upstream_branch(&self) -> &str {
        self.upstream_branch.as_deref().unwrap_or(&self.branch)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        // Read the version on its own first. A config from a future build will
        // almost certainly trip `deny_unknown_fields` on some inner field, and
        // "unknown field `foo`" is a much worse error than being told the
        // schema version is not supported.
        #[derive(Deserialize)]
        struct JustVersion {
            version: Option<u32>,
        }
        let probe: JustVersion =
            toml::from_str(text).unwrap_or(JustVersion { version: None });
        match probe.version {
            Some(v) if v != SCHEMA_VERSION => {
                bail!("config schema version {v} is not supported; this build understands version {SCHEMA_VERSION}")
            }
            None => bail!("config is missing `version`; expected `version = {SCHEMA_VERSION}`"),
            _ => {}
        }

        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.forge.url.trim().is_empty() {
            bail!("forge.url is empty");
        }
        if !self.forge.url.starts_with("http://") && !self.forge.url.starts_with("https://") {
            bail!(
                "forge.url must start with http:// or https:// (got {:?})",
                self.forge.url
            );
        }
        if self.forge.owner.trim().is_empty() {
            bail!("forge.owner is empty");
        }
        if self.defaults.sync_branch.trim().is_empty() {
            bail!("defaults.sync_branch is empty");
        }
        if self.defaults.boundary_file.trim().is_empty() {
            bail!("defaults.boundary_file is empty");
        }
        if self.defaults.boundary_file.starts_with('/') {
            bail!(
                "defaults.boundary_file must be relative to the repository root (got {:?})",
                self.defaults.boundary_file
            );
        }
        if self.forks.is_empty() {
            bail!("no [[fork]] entries; there is nothing to sync");
        }

        let mut seen = HashSet::new();
        for fork in &self.forks {
            if fork.repo.trim().is_empty() {
                bail!("a [[fork]] has an empty repo");
            }
            if !seen.insert(fork.repo.as_str()) {
                bail!("fork {:?} is listed more than once", fork.repo);
            }
            if fork.upstream.trim().is_empty() {
                bail!("fork {:?} has an empty upstream", fork.repo);
            }
            if fork.branch.trim().is_empty() {
                bail!("fork {:?} has an empty branch", fork.repo);
            }
            // The sync branch is force-pushed on every run. Targeting it would
            // destroy the fork on the first sync.
            if fork.branch == self.defaults.sync_branch {
                bail!(
                    "fork {:?} targets {:?}, which is the sync branch itself; it is force-pushed every run",
                    fork.repo,
                    fork.branch
                );
            }
        }
        Ok(())
    }

    /// The forge base URL without a trailing slash, so callers can join paths
    /// without producing a double slash.
    pub fn forge_url(&self) -> &str {
        self.forge.url.trim_end_matches('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
version = 1

[forge]
kind = "gitea"
url = "https://gitea.example.com"
owner = "my-org"

[[fork]]
repo = "codex"
upstream = "https://github.com/openai/codex.git"
branch = "main"
"#;

    #[test]
    fn a_minimal_config_parses_and_fills_in_the_defaults() {
        let config = Config::parse(MINIMAL).expect("should parse");
        assert_eq!(config.forge.kind, ForgeKind::Gitea);
        assert_eq!(config.forge.token_env, "WEIR_TOKEN");
        assert_eq!(config.defaults.sync_branch, "upstream-sync");
        assert_eq!(config.defaults.boundary_file, ".upstream-sync");
        assert_eq!(config.forks.len(), 1);
        assert!(config.forks[0].keep_removed.is_empty());
    }

    #[test]
    fn the_upstream_branch_defaults_to_the_branch_we_target() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.forks[0].upstream_branch(), "main");
    }

    #[test]
    fn the_upstream_branch_can_differ_from_the_one_we_target() {
        let text = format!("{MINIMAL}upstream_branch = \"master\"\n");
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.forks[0].upstream_branch(), "master");
        assert_eq!(config.forks[0].branch, "main");
    }

    #[test]
    fn forks_may_disagree_about_their_base_branch() {
        let text = format!(
            "{MINIMAL}\n[[fork]]\nrepo = \"dokploy\"\nupstream = \"https://github.com/Dokploy/dokploy.git\"\nbranch = \"canary\"\n"
        );
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.forks[0].branch, "main");
        assert_eq!(config.forks[1].branch, "canary");
    }

    #[test]
    fn a_missing_version_is_named_rather_than_guessed_at() {
        let text = MINIMAL.replace("version = 1\n", "");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("missing `version`"), "{err}");
    }

    #[test]
    fn a_future_schema_version_says_so_instead_of_blaming_a_field() {
        let text = MINIMAL.replace("version = 1", "version = 2");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("version 2 is not supported"), "{err}");
    }

    #[test]
    fn a_misspelled_key_is_rejected_rather_than_silently_ignored() {
        let text = MINIMAL.replace("branch = \"main\"", "branchh = \"main\"");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn a_config_with_no_forks_is_pointless_and_says_so() {
        let text = MINIMAL
            .split("[[fork]]")
            .next()
            .expect("there is a prefix")
            .to_string();
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("nothing to sync"), "{err}");
    }

    #[test]
    fn the_same_fork_may_not_be_listed_twice() {
        let text = format!("{MINIMAL}\n[[fork]]\nrepo = \"codex\"\nupstream = \"https://github.com/openai/codex.git\"\nbranch = \"main\"\n");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn a_fork_may_not_target_the_sync_branch_because_that_is_force_pushed() {
        let text = MINIMAL.replace("branch = \"main\"", "branch = \"upstream-sync\"");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("force-pushed"), "{err}");
    }

    #[test]
    fn a_forge_url_without_a_scheme_is_rejected() {
        let text = MINIMAL.replace("https://gitea.example.com", "gitea.example.com");
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("http://"), "{err}");
    }

    #[test]
    fn a_trailing_slash_on_the_forge_url_does_not_produce_a_double_slash() {
        let text = MINIMAL.replace("https://gitea.example.com", "https://gitea.example.com/");
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.forge_url(), "https://gitea.example.com");
    }

    #[test]
    fn an_absolute_boundary_file_is_rejected() {
        let text = MINIMAL.replace(
            "[[fork]]",
            "[defaults]\nboundary_file = \"/etc/upstream-sync\"\n\n[[fork]]",
        );
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(err.contains("relative to the repository root"), "{err}");
    }

    #[test]
    fn forgejo_is_accepted_because_it_answers_the_same_api() {
        let text = MINIMAL.replace("kind = \"gitea\"", "kind = \"forgejo\"");
        assert_eq!(Config::parse(&text).unwrap().forge.kind, ForgeKind::Forgejo);
    }

    #[test]
    fn secrets_are_named_rather_than_carried() {
        let text = format!("{MINIMAL}\n[[notify]]\nkind = \"telegram\"\n");
        let config = Config::parse(&text).unwrap();
        match &config.notify[0] {
            Notify::Telegram { token_env, chat_env } => {
                assert_eq!(token_env, "TELEGRAM_BOT_TOKEN");
                assert_eq!(chat_env, "TELEGRAM_CHAT_ID");
            }
        }
    }
}
