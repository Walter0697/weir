//! Telegram, via the bot API.

use super::Notifier;
use anyhow::{bail, Context, Result};

pub struct Telegram {
    token: String,
    chat_id: String,
    client: reqwest::blocking::Client,
}

impl Telegram {
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            token: token.into(),
            chat_id: chat_id.into(),
            // Short, because this is a courtesy at the end of a run that has
            // already done its work. Nothing should wait long for it.
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .context("building the HTTP client")?,
        })
    }
}

impl Notifier for Telegram {
    fn name(&self) -> &'static str {
        "telegram"
    }

    fn send(&self, message: &str) -> Result<()> {
        // The token is in the path because that is the API's design, not a
        // choice — so it must never be echoed. The error below deliberately
        // reports the status and body without the URL.
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let response = self
            .client
            .post(url)
            .form(&[
                ("chat_id", self.chat_id.as_str()),
                ("text", message),
                // Plain text: a commit subject is entirely capable of holding
                // an underscore or an asterisk, and a markdown parse failure
                // would lose the whole notification.
                ("disable_web_page_preview", "true"),
            ])
            .send()
            .context("sending a Telegram message")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("Telegram returned {status}: {body}");
        }
        Ok(())
    }
}
