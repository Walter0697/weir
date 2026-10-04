//! The Gitea API, which Forgejo also answers.
//!
//! Thin on purpose: everything worth getting wrong lives in the parent module
//! as pure functions. This file only moves JSON.

use super::{Description, Discovered, Forge, PullRequest};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::time::Duration;

const MUTATION_ATTEMPTS: usize = 3;

pub struct Gitea {
    base: String,
    owner: String,
    token: String,
    client: reqwest::blocking::Client,
}

impl Gitea {
    pub fn new(base_url: &str, owner: &str, token: &str) -> Result<Self> {
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            owner: owner.to_string(),
            token: token.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("building the HTTP client")?,
        })
    }

    fn url(&self, repo: &str, tail: &str) -> String {
        format!("{}/api/v1/repos/{}/{repo}{tail}", self.base, self.owner)
    }

    fn send_once(&self, request: reqwest::blocking::RequestBuilder, what: &str) -> Result<String> {
        // The token goes in a header rather than the URL so it stays out of
        // proxy logs and out of anything that echoes the address back.
        let response = request
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json")
            .send()
            .with_context(|| format!("{what}: request failed"))?;

        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            // 403 here is almost always a token scope problem, and saying so
            // saves a long detour through permissions screens.
            let hint = if status.as_u16() == 403 {
                " (the token may lack the write:repository scope)"
            } else {
                ""
            };
            bail!("{what}: forge returned {status}{hint}: {body}");
        }
        Ok(body)
    }

    fn send_mutation<F>(&self, request: F, what: &str) -> Result<String>
    where
        F: Fn() -> reqwest::blocking::RequestBuilder,
    {
        let mut last_error = None;
        for attempt in 0..MUTATION_ATTEMPTS {
            match self.send_once(request(), what) {
                Ok(body) => return Ok(body),
                Err(error) if retryable_transport_error(&error) => {
                    last_error = Some(error);
                    if attempt + 1 < MUTATION_ATTEMPTS {
                        retry_delay(attempt);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("mutation attempts always record an error"))
    }

    fn pull_request(&self, repo: &str, number: u64) -> Result<String> {
        self.send_once(
            self.client.get(self.url(repo, &format!("/pulls/{number}"))),
            &format!("reading pull request #{number} on {repo}"),
        )
    }
}

fn retryable_transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|error| error.is_timeout() || error.is_connect())
    })
}

fn retry_delay(attempt: usize) {
    std::thread::sleep(Duration::from_millis(250 * 2_u64.pow(attempt as u32)));
}

fn response_matches_description(body: &str, what: &Description) -> Result<bool> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("parsing the pull request")?;
    Ok(
        value.get("title").and_then(serde_json::Value::as_str) == Some(what.title.as_str())
            && value.get("body").and_then(serde_json::Value::as_str) == Some(what.body.as_str()),
    )
}

fn parse_pr(body: &str, what: &str) -> Result<PullRequest> {
    let value: serde_json::Value =
        serde_json::from_str(body).with_context(|| format!("{what}: parsing the response"))?;
    let number = value
        .get("number")
        .and_then(serde_json::Value::as_u64)
        .with_context(|| format!("{what}: response has no pull request number"))?;
    let url = value
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(PullRequest { number, url })
}

impl Forge for Gitea {
    fn find_open(&self, repo: &str, head: &str) -> Result<Option<PullRequest>> {
        let body = self.send_once(
            self.client
                .get(self.url(repo, "/pulls?state=open&limit=50")),
            &format!("listing open pull requests for {repo}"),
        )?;
        let list: Vec<serde_json::Value> = serde_json::from_str(&body)
            .with_context(|| format!("parsing open pull requests for {repo}"))?;

        // Matched on the head ref rather than the title, which a human may have
        // edited, or the author, which changes if the token is reissued.
        for pr in list {
            let matches = pr
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(serde_json::Value::as_str)
                == Some(head);
            if matches {
                let number = pr
                    .get("number")
                    .and_then(serde_json::Value::as_u64)
                    .context("open pull request has no number")?;
                let url = pr
                    .get("html_url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                return Ok(Some(PullRequest { number, url }));
            }
        }
        Ok(None)
    }

    fn create(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        what: &Description,
    ) -> Result<PullRequest> {
        let payload = json!({
            "title": what.title,
            "head": head,
            "base": base,
            "body": what.body,
        })
        .to_string();
        let what_text = format!("opening a pull request for {repo}");
        let mut last_error = None;
        for attempt in 0..MUTATION_ATTEMPTS {
            if attempt > 0 {
                if let Some(pr) = self.find_open(repo, head)? {
                    return Ok(pr);
                }
                retry_delay(attempt - 1);
            }
            match self.send_once(
                self.client
                    .post(self.url(repo, "/pulls"))
                    .body(payload.clone()),
                &what_text,
            ) {
                Ok(body) => return parse_pr(&body, "opening a pull request"),
                Err(error) if retryable_transport_error(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        if let Some(pr) = self.find_open(repo, head)? {
            return Ok(pr);
        }
        Err(last_error.expect("creation attempts always record an error"))
    }

    fn update(&self, repo: &str, number: u64, what: &Description) -> Result<()> {
        let payload = json!({ "title": what.title, "body": what.body }).to_string();
        let what_text = format!("refreshing pull request #{number} on {repo}");
        match self.send_mutation(
            || {
                self.client
                    .patch(self.url(repo, &format!("/pulls/{number}")))
                    .body(payload.clone())
            },
            &what_text,
        ) {
            Ok(_) => Ok(()),
            Err(update_error) => match self
                .pull_request(repo, number)
                .and_then(|body| response_matches_description(&body, what))
            {
                Ok(true) => Ok(()),
                Ok(false) => Err(update_error),
                Err(reconcile_error) => Err(update_error.context(format!(
                    "reconciliation after an ambiguous update failed: {reconcile_error:#}"
                ))),
            },
        }
    }

    fn discover(&self) -> Result<Vec<Discovered>> {
        // An owner is either an organisation or a user and the API separates
        // them, so try one and fall back rather than making the caller know
        // which kind theirs is.
        let body = match self.send_once(
            self.client.get(format!(
                "{}/api/v1/orgs/{}/repos?limit=100",
                self.base, self.owner
            )),
            "listing organisation repositories",
        ) {
            Ok(body) => body,
            Err(_) => self.send_once(
                self.client.get(format!(
                    "{}/api/v1/users/{}/repos?limit=100",
                    self.base, self.owner
                )),
                "listing repositories",
            )?,
        };

        let repos: Vec<serde_json::Value> =
            serde_json::from_str(&body).context("parsing the repository list")?;
        let mut found: Vec<Discovered> = repos
            .into_iter()
            .filter_map(|repo| {
                let name = repo.get("name")?.as_str()?.to_string();
                let default_branch = repo
                    .get("default_branch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("main")
                    .to_string();
                let upstream = repo
                    .get("original_url")
                    .and_then(serde_json::Value::as_str)
                    .filter(|u| !u.is_empty())
                    .map(|u| {
                        // `original_url` is the browse URL; git wants the clone
                        // URL, which for the forges people migrate from is the
                        // same address with `.git`.
                        if u.ends_with(".git") {
                            u.to_string()
                        } else {
                            format!("{u}.git")
                        }
                    });
                Some(Discovered {
                    name,
                    default_branch,
                    archived: repo
                        .get("archived")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    mirror: repo
                        .get("mirror")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    upstream,
                })
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    fn close(&self, repo: &str, number: u64) -> Result<()> {
        self.send_once(
            self.client
                .patch(self.url(repo, &format!("/pulls/{number}")))
                .body(json!({ "state": "closed" }).to_string()),
            &format!("closing pull request #{number} on {repo}"),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let gitea = Gitea::new("https://forge.example/", "org", "t").unwrap();
        assert_eq!(
            gitea.url("codex", "/pulls"),
            "https://forge.example/api/v1/repos/org/codex/pulls"
        );
    }

    #[test]
    fn a_created_pull_request_is_read_back_by_number() {
        let pr = parse_pr(
            r#"{"number": 15, "html_url": "https://forge.example/org/codex/pulls/15"}"#,
            "test",
        )
        .unwrap();
        assert_eq!(pr.number, 15);
        assert_eq!(pr.url, "https://forge.example/org/codex/pulls/15");
    }

    #[test]
    fn a_response_without_a_number_is_an_error_rather_than_a_zero() {
        assert!(parse_pr(r#"{"message": "no permission"}"#, "test").is_err());
    }

    #[test]
    fn a_fetched_pull_request_matches_the_description_after_an_ambiguous_update() {
        let what = Description {
            title: "sync title".into(),
            body: "sync body".into(),
        };

        assert!(response_matches_description(
            r#"{"title":"sync title","body":"sync body"}"#,
            &what
        )
        .unwrap());
    }

    #[test]
    fn a_fetched_pull_request_with_old_content_does_not_confirm_an_update() {
        let what = Description {
            title: "sync title".into(),
            body: "sync body".into(),
        };

        assert!(
            !response_matches_description(r#"{"title":"old title","body":"old body"}"#, &what)
                .unwrap()
        );
    }
}
