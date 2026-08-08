//! `hanzo billing` — the prepaid wallet, read.
//!
//! One verb against the money plane cloud already serves: `balance` is
//! `GET /v1/billing/balance`, and any signed-in identity reads its OWN.
//!
//! THE CLI SENDS ONLY A BEARER. The endpoint derives the tenant SERVER-SIDE
//! from the JWT `owner` claim (cloud's validated principal → commerce's
//! `middleware.GetOrganization`), so there is no org flag and no `X-Org-Id`:
//! nothing here can name — let alone forge — the tenant whose ledger it reads.
//! That is also why there is no billing selector: `hanzo auth use` moves the money
//! because it moves the identity, and `owner` IS the billing key.
//!
//! There WAS a second verb. `deposit` posted to `/v1/billing/deposit`, a route
//! hanzoai/cloud has never served — the mint is `POST /v1/billing/topup` and
//! `POST /v1/billing/crypto/deposit`, both of which reach a person as generated
//! commands. It survived because the drift gate only ever read the generated
//! table, so a route only a hand-written command sends was ruled on by nobody;
//! `driftgate::sent` now reads them too.
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;

use crate::commands::network;
use crate::config::Config;
use crate::iam::identity::Identity;
use crate::iam::{paths, store};

/// WHO is asking, WITH what, and WHERE — resolved once, together.
///
/// The identity travels beside the credential because a refusal has to name the
/// principal that was refused; re-deriving it anywhere else is how the two drift
/// apart. Both come from [`store::active_token`] — THE one way any command
/// resolves a credential — so `hanzo billing` bills exactly the identity
/// `hanzo auth show` names, and `hanzo auth use` moves it.
struct Caller {
    id: Identity,
    token: String,
    api: String,
}

impl Caller {
    async fn resolve(cfg: &mut Config) -> Result<Self> {
        let api = network::active(cfg).api.trim_end_matches('/').to_string();
        let (id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND).await?
            .ok_or_else(|| anyhow!("not signed in — run `hanzo auth login` first"))?;
        Ok(Self { id, token: tok.access_token, api })
    }

    /// One authenticated call to the billing plane.
    ///
    /// Returns the STATUS beside the body because a money verdict is a VALUE the
    /// caller must read, not a failure to flatten into a string: a 403 from the
    /// mint gate is the server answering correctly. Only a transport fault is an
    /// `Err`. Sends the bearer and nothing else — no org, ever.
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let url = format!("{}{path}", self.api);
        let mut req = Client::new().request(method, &url).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.with_context(|| format!("request {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if text.trim().is_empty() {
            return Ok((status, Value::Null));
        }
        // A non-JSON body (an ingress's HTML 502) is still the server's answer:
        // keep it as text rather than failing to parse and losing the reason.
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::String(text))))
    }
}

/// The server's own words for a response, from either error envelope in use:
/// zip's `{"error":"…"}` (cloud) and commerce's `{"error":{"message":"…"}}`.
/// Falls back to the verbatim body — we print what the server said, never a
/// message we made up on its behalf.
fn message(body: &Value) -> String {
    let err = body.get("error");
    if let Some(s) = err.and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(s) = err.and_then(|e| e.get("message")).and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(s) = body.get("message").and_then(Value::as_str) {
        return s.to_string();
    }
    match body {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

/// Render the wallet from commerce's `{balance,holds,available}` cents wire.
///
/// A balance we cannot READ is UNKNOWN, and unknown is not "broke": if the body
/// carries none of the fields, this fails rather than printing a zero the server
/// never sent — the same rule cloud enforces on its own read path.
fn render_balance(v: &Value) -> Result<String> {
    let fields: Vec<(&str, i64)> = ["available", "balance", "holds"]
        .iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_i64).map(|n| (*k, n)))
        .collect();
    if fields.is_empty() {
        bail!("unreadable balance — the server sent no amount: {v}");
    }
    // Cents, as the ledger states them. Rendering major units would need the
    // currency's exponent, which this wire does not always carry — and a guessed
    // decimal point on money is a lie, not a convenience.
    let cur = v
        .get("currency")
        .and_then(Value::as_str)
        .map(|c| format!(" {c}"))
        .unwrap_or_default();
    Ok(fields
        .iter()
        .map(|(k, n)| format!("  {:<10} {}{cur}", k, format!("{n} cents").bold()))
        .collect::<Vec<_>>()
        .join("\n"))
}

impl Caller {
    /// Read this identity's own prepaid wallet.
    async fn read_balance(&self) -> Result<()> {
        let (status, body) = self.call(Method::GET, "/v1/billing/balance", None).await?;
        if !status.is_success() {
            bail!("billing balance refused ({status}): {}", message(&body));
        }
        println!("{}", format!("{} wallet", self.id).dimmed());
        println!("{}", render_balance(&body)?);
        Ok(())
    }

}

/// `hanzo billing balance` — the ACTIVE identity's own prepaid wallet.
pub async fn balance(cfg: &mut Config) -> Result<()> {
    Caller::resolve(cfg).await?.read_balance().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::testmock::MockCloud;

    fn id(s: &str) -> Identity {
        // Derived from claims, as everywhere else — there is no other way to
        // build one, which is the point.
        let (owner, name) = s.split_once('/').unwrap();
        Identity::from_access_token(&crate::iam::identity::testjwt::jwt(owner, name)).unwrap()
    }

    fn caller(api: &str, who: &str) -> Caller {
        Caller { id: id(who), token: "TOK123".into(), api: api.to_string() }
    }

    // ---- the wire ----------------------------------------------------------

    #[tokio::test]
    async fn balance_reads_the_wallet_and_never_sends_an_org() {
        let mock = MockCloud::start().await;
        let (status, body) = caller(&mock.base_url(), "hanzo/z")
            .call(Method::GET, "/v1/billing/balance", None)
            .await
            .unwrap();

        assert!(status.is_success());
        assert!(render_balance(&body).unwrap().contains("125000 cents"));
        let r = &mock.requests()[0];
        assert_eq!(r.path, "/v1/billing/balance");
        assert_eq!(r.header("authorization").as_deref(), Some("Bearer TOK123"));
        assert!(r.header("x-org-id").is_none(), "CLI must not send X-Org-Id");
    }

    // ---- reading the server honestly ---------------------------------------

    /// Both envelopes in production: zip's string (cloud) and commerce's object.
    #[test]
    fn the_servers_own_words_are_read_from_either_error_envelope() {
        let commerce = serde_json::json!({"error": {"type": "api-error", "message": "This operation requires platform-administrator or internal-service credentials."}});
        assert!(message(&commerce).starts_with("This operation requires platform-administrator"));

        let zip = serde_json::json!({"error": "sign in to view billing"});
        assert_eq!(message(&zip), "sign in to view billing");

        // An ingress's non-JSON body is still the server's answer.
        assert_eq!(message(&Value::String("502 Bad Gateway".into())), "502 Bad Gateway");
    }

    /// A balance that cannot be read is UNKNOWN — and unknown is not "broke".
    /// It must never render as a zero the server never sent.
    #[test]
    fn an_unreadable_balance_is_never_rendered_as_zero() {
        let err = render_balance(&serde_json::json!({"unexpected": "shape"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unreadable balance"), "{err}");
        assert!(!err.contains('0'), "must not imply a zero balance: {err}");
    }

    #[test]
    fn a_balance_renders_only_the_amounts_the_server_sent() {
        let out = render_balance(&serde_json::json!({"available": 125_000, "holds": 0})).unwrap();
        assert!(out.contains("125000 cents"));
        assert!(out.contains("holds"));
        assert!(!out.contains("balance"), "must not invent a field: {out}");
    }
}
