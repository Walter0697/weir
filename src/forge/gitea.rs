//! The Gitea API, which Forgejo also answers.
//!
//! Thin on purpose: everything worth getting wrong lives in the parent module
//! as pure functions. This file only moves JSON.

use super::{Description, Forge, PullRequest};
use anyhow::{bail, Context, Result};
use serde_json::json;

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

    fn send(&self, request: reqwest::blocking::RequestBuilder, what: &str) -> Result<String> {
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
        let body = self.send(
            self.client.get(self.url(repo, "/pulls?state=open&limit=50")),
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
        let body = self.send(
            self.client.post(self.url(repo, "/pulls")).body(
                json!({
                    "title": what.title,
                    "head": head,
                    "base": base,
                    "body": what.body,
                })
                .to_string(),
            ),
            &format!("opening a pull request for {repo}"),
        )?;
        parse_pr(&body, "opening a pull request")
    }

    fn update(&self, repo: &str, number: u64, what: &Description) -> Result<()> {
        self.send(
            self.client
                .patch(self.url(repo, &format!("/pulls/{number}")))
                .body(json!({ "title": what.title, "body": what.body }).to_string()),
            &format!("refreshing pull request #{number} on {repo}"),
        )?;
        Ok(())
    }

    fn close(&self, repo: &str, number: u64) -> Result<()> {
        self.send(
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
}
