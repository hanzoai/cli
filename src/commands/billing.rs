//! `hanzo billing` — the prepaid wallet: read it, and mint into it.
//!
//! Two verbs against the money plane cloud already serves:
//!
//! | verb      | wire                       | who may                           |
//! |-----------|----------------------------|-----------------------------------|
//! | `balance` | `GET  /v1/billing/balance` | any signed-in identity — its OWN  |
//! | `deposit` | `POST /v1/billing/deposit` | the mint principal — server's call |
//!
//! THE CLI SENDS ONLY A BEARER. Both endpoints derive the tenant SERVER-SIDE
//! from the JWT `owner` claim (cloud's validated principal → commerce's
//! `middleware.GetOrganization`), so there is no org flag and no `X-Org-Id`:
//! nothing here can name — let alone forge — the tenant whose ledger it touches.
//! That is also why there is no billing selector: `hanzo switch` moves the money
//! because it moves the identity, and `owner` IS the billing key.
//!
//! WHO MAY MINT IS THE SERVER'S CALL, ALWAYS. `deposit` is gated by commerce's
//! `middleware.PlatformOnly` → `MayMintMoney`, which admits the internal service
//! token or `IsSuperAdmin()` ⟺ membership of the reserved `admin` org — read
//! from the token the server itself VERIFIED. This module never evaluates that
//! predicate to decide what to send; once the server has REFUSED, it hands the
//! refusal to [`store::refusal_hint`] — the ONE explainer, shared by every
//! command that can meet a SuperAdmin gate, not a billing special case.
//!
//! NO MONEY POLICY LIVES HERE. The bounds on a deposit (positive, and at most
//! `COMMERCE_DEPOSIT_MAX_CENTS`) are server-authoritative and deploy-tunable, so
//! mirroring them here would only drift and lie. We send what the operator said
//! and print what the server answered — amounts are never defaulted, rounded, or
//! invented.

use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Map, Value};

use crate::commands::network;
use crate::config::Config;
use crate::iam::identity::Identity;
use crate::iam::{paths, store};

/// The deposit request — commerce's `depositRequest` body, exactly.
///
/// `user` is the deposit's BENEFICIARY (the IAM subject the credit lands on),
/// not a tenant selector: commerce namespaces the ledger by the server-derived
/// org and reads `user` as the destination account within it. It is required
/// because the server requires it, and it is NOT defaulted: the subject rule is
/// `account.Payer` (org pool vs `org/name` person, conditioned on claims the CLI
/// cannot see), so computing one here would be a guess — and a guess that drifts
/// from the gate is precisely the bug that funded an account the meter never
/// read. An operator names the beneficiary; we never invent one.
#[derive(Debug, Default)]
pub struct Deposit {
    pub user: String,
    pub cents: i64,
    pub currency: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<String>,
    pub expires_in: Option<u32>,
}

impl Deposit {
    /// The JSON body — carrying ONLY what the operator actually stated.
    ///
    /// An unset option is OMITTED, never sent as an empty string or a zero, so
    /// the server's own defaults (currency `usd`, no expiry) remain the single
    /// source of those values. Pure, so the "we invent nothing" claim is a test
    /// rather than a comment.
    fn body(&self) -> Value {
        let mut m = Map::new();
        m.insert("user".into(), self.user.trim().into());
        m.insert("amount".into(), self.cents.into());
        if let Some(c) = &self.currency {
            m.insert("currency".into(), c.trim().into());
        }
        if let Some(n) = &self.notes {
            m.insert("notes".into(), n.as_str().into());
        }
        if let Some(t) = &self.tags {
            m.insert("tags".into(), t.as_str().into());
        }
        if let Some(d) = self.expires_in {
            m.insert("expiresIn".into(), d.into());
        }
        Value::Object(m)
    }
}

/// WHO is asking, WITH what, and WHERE — resolved once, together.
///
/// The identity travels beside the credential because a refusal has to name the
/// principal that was refused; re-deriving it anywhere else is how the two drift
/// apart. Both come from [`store::active_token`] — THE one way any command
/// resolves a credential — so `hanzo billing` bills exactly the identity
/// `hanzo whoami` names, and `hanzo switch` moves it.
struct Caller {
    id: Identity,
    token: String,
    api: String,
    /// The identities we hold, carried as a VALUE so explaining a refusal needs
    /// no config lookup at the point of failure (`store::refusal_hint`).
    held: Vec<Identity>,
}

impl Caller {
    fn resolve(cfg: &mut Config) -> Result<Self> {
        let api = network::active(cfg).api.trim_end_matches('/').to_string();
        let (id, tok) = store::active_token(cfg, paths::DEFAULT_BRAND)?
            .ok_or_else(|| anyhow!("not signed in — run `hanzo login` first"))?;
        let held = store::list(cfg, paths::DEFAULT_BRAND);
        Ok(Self { id, token: tok.access_token, api, held })
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

/// Render the deposit receipt — only fields the server actually returned.
fn render_receipt(v: &Value) -> String {
    let mut out = Vec::new();
    for k in ["transactionId", "user", "amount", "currency", "type", "tags", "expiresAt", "txHash"] {
        match v.get(k) {
            Some(Value::String(s)) if !s.is_empty() => out.push(format!("  {k:<14} {s}")),
            Some(Value::Number(n)) => out.push(format!("  {k:<14} {n}")),
            _ => {}
        }
    }
    out.join("\n")
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

    /// Post a deposit, and make a refusal actionable.
    async fn post_deposit(&self, d: &Deposit) -> Result<()> {
        let (status, body) = self.call(Method::POST, "/v1/billing/deposit", Some(d.body())).await?;

        if status == StatusCode::FORBIDDEN {
            // The server has refused. ONLY NOW do we read our own identity — to
            // explain the refusal, never to have pre-empted it. The explainer is
            // shared, because a SuperAdmin gate is not a billing idea.
            let hint = store::refusal_hint(&self.id, &self.held).unwrap_or_default();
            bail!("deposit refused ({status}): {}{hint}", message(&body));
        }
        if !status.is_success() {
            bail!("deposit refused ({status}): {}", message(&body));
        }
        println!("{}", "deposit posted".green());
        println!("{}", render_receipt(&body));
        Ok(())
    }
}

/// `hanzo billing balance` — the ACTIVE identity's own prepaid wallet.
pub async fn balance(cfg: &mut Config) -> Result<()> {
    Caller::resolve(cfg)?.read_balance().await
}

/// `hanzo billing deposit` — the money-in primitive, gated server-side.
pub async fn deposit(cfg: &mut Config, d: Deposit) -> Result<()> {
    Caller::resolve(cfg)?.post_deposit(&d).await
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

    /// A caller who holds BOTH of z@hanzo.ai's identities — the real fleet
    /// state, and the one in which a refusal has something useful to say.
    fn caller(api: &str, who: &str) -> Caller {
        Caller {
            id: id(who),
            token: "TOK123".into(),
            api: api.to_string(),
            held: vec![id("hanzo/z"), id("admin/z")],
        }
    }

    fn a_deposit() -> Deposit {
        Deposit { user: "hanzo".into(), cents: 5000, ..Default::default() }
    }

    // ---- the body states only what the operator stated ----------------------

    /// We never invent money or a currency: an option the operator did not set
    /// is ABSENT from the wire, so the server's own default is the only default.
    #[test]
    fn the_body_omits_every_field_the_operator_did_not_set() {
        let b = a_deposit().body();
        assert_eq!(b["user"], "hanzo");
        assert_eq!(b["amount"], 5000);
        for absent in ["currency", "notes", "tags", "expiresIn"] {
            assert!(b.get(absent).is_none(), "{absent} was invented: {b}");
        }
    }

    #[test]
    fn the_body_carries_every_field_the_operator_did_set() {
        let b = Deposit {
            user: "hanzo/z".into(),
            cents: 12_345,
            currency: Some("usd".into()),
            notes: Some("settlement".into()),
            tags: Some("credit".into()),
            expires_in: Some(30),
        }
        .body();
        assert_eq!(b["user"], "hanzo/z");
        assert_eq!(b["amount"], 12_345);
        assert_eq!(b["currency"], "usd");
        assert_eq!(b["notes"], "settlement");
        assert_eq!(b["tags"], "credit");
        assert_eq!(b["expiresIn"], 30);
    }

    /// THE INVARIANT: the org is the gateway's to derive from the JWT `owner`.
    /// The CLI never names a tenant — not in the body, not anywhere.
    #[test]
    fn the_body_never_carries_an_org() {
        let b = Deposit { user: "hanzo".into(), cents: 1, ..Default::default() }.body();
        for banned in ["org", "owner", "orgId", "tenant"] {
            assert!(b.get(banned).is_none(), "{banned} must never be sent: {b}");
        }
    }

    // ---- the wire ----------------------------------------------------------

    #[tokio::test]
    async fn deposit_sends_only_a_bearer_and_never_an_org() {
        let mock = MockCloud::start().await;
        caller(&mock.base_url(), "admin/z").post_deposit(&a_deposit()).await.unwrap();

        let r = &mock.requests()[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/v1/billing/deposit", "/v1 only, no /api/ prefix");
        assert_eq!(r.header("authorization").as_deref(), Some("Bearer TOK123"));
        assert!(r.header("x-org-id").is_none(), "CLI must not send X-Org-Id");
        assert_eq!(r.json()["amount"], 5000);
    }

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

    // ---- THE INCIDENT: a 403 names the identity and the way out -------------

    /// The deposit-403 loop, closed. The server refuses the org-owner token
    /// exactly as `middleware.PlatformOnly` does in production; the CLI must
    /// surface WHO it was, WHY that was refused, and the ONE command that fixes
    /// it — never a bare "403".
    #[tokio::test]
    async fn a_refused_deposit_names_the_identity_and_suggests_the_switch() {
        let mock = MockCloud::start_deposit_refused().await;
        let err = caller(&mock.base_url(), "hanzo/z")
            .post_deposit(&a_deposit())
            .await
            .unwrap_err()
            .to_string();

        // The server's verbatim reason, never a message we made up.
        assert!(err.contains("platform-administrator"), "server's words missing: {err}");
        // WHO we were.
        assert!(err.contains("hanzo/z"), "must name the refused identity: {err}");
        // WHY, and the WAY OUT.
        assert!(err.contains("admin"), "must name the reserved org: {err}");
        assert!(err.contains("hanzo switch admin/z"), "must be actionable: {err}");
        // Never the credential.
        assert!(!err.contains("TOK123"), "token must never be printed: {err}");
    }

    /// The request goes out REGARDLESS of what our own claims say — the local
    /// decode is never an authz decision. An org-owner deposit is attempted and
    /// REFUSED BY THE SERVER; it is never refused client-side.
    #[tokio::test]
    async fn the_client_never_gates_the_mint_itself() {
        let mock = MockCloud::start_deposit_refused().await;
        let _ = caller(&mock.base_url(), "hanzo/z").post_deposit(&a_deposit()).await;
        assert_eq!(mock.requests().len(), 1, "the server must be the one to refuse");
        assert_eq!(mock.requests()[0].path, "/v1/billing/deposit");
    }

    /// A SuperAdmin refused is NOT an identity problem, so the shared explainer
    /// stays silent and the server's own reason stands alone — no misleading
    /// "switch to the org you are already in". (The explainer's own cases are
    /// proven in `iam::store`; this pins that billing WIRES it.)
    #[tokio::test]
    async fn a_superadmin_refusal_surfaces_the_server_reason_and_no_switch() {
        let mock = MockCloud::start_deposit_refused().await;
        let err = caller(&mock.base_url(), "admin/z")
            .post_deposit(&a_deposit())
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("platform-administrator"), "server's words missing: {err}");
        assert!(!err.contains("hanzo switch"), "must not suggest a pointless switch: {err}");
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
