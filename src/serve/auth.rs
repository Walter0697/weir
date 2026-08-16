//! A shared-token gate, for when this is reachable from more than localhost.
//!
//! Deliberately the smallest thing that works: one token, held in an
//! environment variable, presented once and kept in a cookie. There are no
//! accounts because there is nothing to distinguish between — everyone who can
//! reach this can do everything.
//!
//! It is off unless `WEIR_UI_TOKEN` is set. On loopback that is the right
//! default; the moment the bind address is not loopback, `serve` says so at
//! startup, and says it louder when there is no token.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use maud::{html, DOCTYPE};

const COOKIE: &str = "weir_session";

#[derive(Clone)]
pub struct Auth {
    token: Option<String>,
}

impl Auth {
    /// Reads the token from the environment. Absent or blank means open.
    pub fn from_env() -> Self {
        Self {
            token: std::env::var("WEIR_UI_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        }
    }

    pub fn required(&self) -> bool {
        self.token.is_some()
    }

    /// Compared without an early return, so the time taken does not reveal how
    /// much of a guess was right.
    fn matches(&self, candidate: &str) -> bool {
        let Some(token) = &self.token else {
            return true;
        };
        if token.len() != candidate.len() {
            return false;
        }
        token
            .bytes()
            .zip(candidate.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

fn cookie_value(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

pub async fn guard(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    if !auth.required() {
        return next.run(request).await;
    }
    // The login form has to be reachable or there is no way in, and its icon
    // with it — a sign-in page that redirects its own favicon to itself is a
    // small thing done badly.
    if matches!(
        request.uri().path(),
        "/login" | "/icon.png" | "/favicon.png"
    ) {
        return next.run(request).await;
    }
    match cookie_value(&request) {
        Some(value) if auth.matches(&value) => next.run(request).await,
        _ => Redirect::to("/login").into_response(),
    }
}

pub async fn login_page() -> impl IntoResponse {
    page(None)
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    token: String,
}

pub async fn login(
    State(auth): State<Auth>,
    axum::Form(form): axum::Form<LoginForm>,
) -> impl IntoResponse {
    if !auth.matches(form.token.trim()) {
        return page(Some("That token is not right.")).into_response();
    }
    // HttpOnly so a page cannot read it back, Lax so a link from elsewhere
    // cannot act on your behalf. Not marked Secure: this is commonly served
    // over plain HTTP on a home network, and a Secure cookie would simply
    // never be stored there.
    (
        [(
            header::SET_COOKIE,
            format!(
                "{COOKIE}={}; Path=/; HttpOnly; SameSite=Lax",
                form.token.trim()
            ),
        )],
        Redirect::to("/"),
    )
        .into_response()
}

fn page(problem: Option<&str>) -> Response {
    let body = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "weir — sign in" }
                link rel="icon" type="image/png" href="/favicon.png";
                style {
                    r#":root{color-scheme:light dark;--bg:#fbfbfa;--fg:#1a1a1a;--card:#fff;
                       --line:#e3e3e0;--accent:#2f6f4f;--bad:#a33}
                       @media (prefers-color-scheme:dark){:root{--bg:#16181a;--fg:#e8e8e6;
                       --card:#1d2023;--line:#2c2f33;--accent:#6fbf8f;--bad:#e07070}}
                       body{margin:0;min-height:100vh;display:grid;place-items:center;
                       background:var(--bg);color:var(--fg);
                       font:15px/1.55 ui-sans-serif,system-ui,sans-serif}
                       form{background:var(--card);border:1px solid var(--line);border-radius:12px;
                       padding:1.5rem;width:min(22rem,92vw)}
                       h1{font-size:1rem;margin:0}
                       .brand{display:flex;align-items:center;gap:.6rem;margin:0 0 1.1rem}
                       .brand img{border-radius:8px;display:block}
                       input{width:100%;box-sizing:border-box;padding:.55rem .7rem;font:inherit;
                       border:1px solid var(--line);border-radius:7px;background:var(--bg);color:var(--fg)}
                       button{margin-top:.9rem;width:100%;padding:.55rem;font:inherit;border-radius:7px;
                       border:1px solid var(--accent);background:var(--accent);color:#fff;cursor:pointer}
                       .bad{color:var(--bad);font-size:.85rem;margin:.6rem 0 0}
                       .muted{opacity:.65;font-size:.82rem;margin:.9rem 0 0}"#
                }
            }
            body {
                form method="post" action="/login" {
                    div class="brand" {
                        img src="/icon.png" alt="" width="34" height="34";
                        h1 { "weir" }
                    }
                    label for="token" { "Access token" }
                    input type="password" id="token" name="token" autofocus;
                    button type="submit" { "Sign in" }
                    @if let Some(problem) = problem { p class="bad" { (problem) } }
                    p class="muted" {
                        "Set on the server as WEIR_UI_TOKEN. This gate exists because anything that \
                         reaches this page can change which repositories get force-pushed."
                    }
                }
            }
        }
    };
    let status = if problem.is_some() {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    (status, Html(body.into_string())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(token: Option<&str>) -> Auth {
        Auth {
            token: token.map(str::to_string),
        }
    }

    #[test]
    fn with_no_token_configured_everything_is_allowed() {
        let auth = auth(None);
        assert!(!auth.required());
        assert!(auth.matches("anything"));
    }

    #[test]
    fn a_configured_token_accepts_only_itself() {
        let auth = auth(Some("correct-horse"));
        assert!(auth.required());
        assert!(auth.matches("correct-horse"));
        assert!(!auth.matches("correct-hors"));
        assert!(!auth.matches("correct-horsee"));
        assert!(!auth.matches(""));
    }

    /// A near miss and a wildly wrong guess must cost the same, or the length
    /// and prefix of the real token leak through timing.
    #[test]
    fn a_wrong_token_of_the_right_length_is_still_wrong() {
        let auth = auth(Some("abcdefgh"));
        assert!(!auth.matches("abcdefgX"));
        assert!(!auth.matches("XXXXXXXX"));
    }
}
