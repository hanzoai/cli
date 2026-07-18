//! `hanzo usage` — the STACKED, per-account balance view.
//!
//! One human holds many principals (`hanzo/z`, `admin/z`) and may also hold raw
//! provider keys (OpenAI/Anthropic). Their balances are DISJOINT: each ledger is
//! its own, billed to its own `owner`, and updates independently. So this is
//! deliberately NOT an aggregate — there is no total. It is a STACK: one row per
//! account, each showing THAT account's own remaining balance, fetched
//! independently with THAT account's own token.
//!
//! DISJOINT ALL THE WAY DOWN. Each account is read on its own request with its
//! own bearer; a failure on one — an expired token (401), a network fault, an
//! unreadable body — becomes THAT row's state and never aborts the fan-out. One
//! account down never blanks the view (`classify` + `render` are pure, so this is
//! a test, not a hope).
//!
//! THE WIRE IS THE SAME ONE `hanzo billing balance` READS: `GET /v1/billing/balance`
//! (cloud `clients/billing/billing.go` `balance` → the co-resident finance ledger),
//! which answers `{balance, holds, available}` in USD **cents** (`currency.Cents`
//! is whole cents; the finance ledger and commerce both denominate in `usd`). The
//! CLI sends ONLY the bearer — the org is the gateway's to derive from the JWT
//! `owner` claim — so a row can only ever read its OWN account.

use anyhow::{bail, Result};
use colored::*;
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;

use crate::commands::network;
use crate::config::Config;
use crate::http;
use crate::iam::identity::Identity;
use crate::iam::provider::{self, Provider};
use crate::iam::{paths, store};

/// The wire path — the SAME handler `hanzo billing balance` reads.
const BALANCE_PATH: &str = "/v1/billing/balance";

/// One account's resolved balance — a pure value, so the whole stacked view
/// renders (and is tested) without a network or a keychain.
#[derive(Debug, Clone, PartialEq)]
enum Balance {
    /// A Hanzo ledger read succeeded. USD cents (see the module money model):
    /// `available` is the spendable "usage left", `balance` the settled amount,
    /// `holds` the reserved amount.
    Read { available: i64, balance: i64, holds: i64 },
    /// A raw provider key (OpenAI/Anthropic/`hk-`) — opaque, with no Hanzo ledger
    /// to read. We show the label, never a fabricated number.
    ProviderKey,
    /// An indexed identity whose credential we do not hold (revoked / wiped).
    NoCredential,
    /// The read failed — the honest reason (sign-in / network / upstream code).
    /// NEVER a rendered zero: an unknown balance is not "broke".
    Unavailable(String),
}

/// One line in the stack — a pure value.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    /// `owner/name` for an identity; the provider slug for a raw provider key.
    account: String,
    /// `<brand> id` for an identity; `provider key` for a raw provider key.
    kind: String,
    /// The active identity (or the active provider) in its axis — marked `*`.
    active: bool,
    balance: Balance,
}

/// One identity's resolved credential, fed to the fan-out. Separates the
/// keychain-touching resolution (in `usage`) from the network fetch (`fetch_rows`,
/// testable without a keychain) — and crucially keeps resolution DISJOINT: a
/// corrupt / mismatched slot is THIS identity's problem, isolated to its row,
/// never an error out of the whole command.
enum Cred {
    /// Fetch this identity's balance with this bearer.
    Token(String),
    /// We hold no credential for this indexed identity (revoked / wiped).
    Missing,
    /// The stored credential could not be read, or self-identifies as another
    /// principal — fail closed to an unavailable ROW, never blank the view.
    Unreadable,
}

/// `hanzo usage` — every account you hold, stacked, each showing its OWN
/// remaining balance fetched with its OWN token.
pub async fn usage(cfg: &mut Config, brand: &str) -> Result<()> {
    let api = network::active(cfg).api.trim_end_matches('/').to_string();

    // Resolve the ACTIVE identity's credential through THE seam — this ALSO runs
    // the one-shot legacy migration, so a pre-multi-identity user is enumerated
    // too, and we reuse its token instead of re-reading the keychain for it.
    let active_pair = store::active_token(cfg, brand)?;
    let active = active_pair.as_ref().map(|(id, _)| id.clone());

    let ids = store::list(cfg, brand);
    let provider_rows = provider_rows(brand)?;

    if ids.is_empty() && provider_rows.is_empty() {
        bail!("not signed in — run `hanzo login` first");
    }

    // Resolve each identity's OWN credential: reuse the active one we already
    // hold, else load THIS identity's own — never the active one's, so a row can
    // never read another identity's account. A `token_for` error (a corrupt slot,
    // or one that self-identifies as another principal) folds into THIS row's
    // `Unreadable` — it must NOT abort the whole view (disjoint all the way down).
    // We never surface the mismatched principal the error names.
    let mut accounts: Vec<(Identity, Cred)> = Vec::with_capacity(ids.len());
    for id in ids {
        let cred = match &active_pair {
            Some((aid, tok)) if *aid == id => Cred::Token(tok.access_token.clone()),
            _ => match store::token_for(cfg, brand, &id) {
                Ok(Some(t)) => Cred::Token(t.access_token),
                Ok(None) => Cred::Missing,
                Err(_) => Cred::Unreadable,
            },
        };
        accounts.push((id, cred));
    }

    // ONE client, reused across every account (connection pool). Each balance is
    // read on its own request; a failure is isolated to its row.
    let client = Client::new();
    let mut rows = fetch_rows(&client, &api, brand, active.as_ref(), &accounts).await;
    rows.extend(provider_rows);

    println!("{}", format!("usage — every {brand} account you hold, stacked").dimmed());
    println!("{}", render(&rows));
    println!(
        "{}",
        "  * = active · amounts are USD, each read with that account's own token".dimmed()
    );
    Ok(())
}

/// The effectful core: fetch every account's balance and build its row. Isolated
/// from the store/keychain (accounts are passed in resolved) so it is tested
/// end-to-end against a mock cloud with no credential store.
async fn fetch_rows(
    client: &Client,
    api: &str,
    brand: &str,
    active: Option<&Identity>,
    accounts: &[(Identity, Cred)],
) -> Vec<Row> {
    let url = format!("{api}{BALANCE_PATH}");
    let mut rows = Vec::with_capacity(accounts.len());
    for (id, cred) in accounts {
        let balance = match cred {
            // The bearer is this identity's OWN; the org is derived server-side.
            Cred::Token(tok) => classify(http::send(client, Method::GET, &url, tok, None::<&Value>).await),
            // No credential held — do not fetch, do not fabricate. Just say so.
            Cred::Missing => Balance::NoCredential,
            // A corrupt / mismatched slot — its OWN row's problem, never the view's.
            Cred::Unreadable => Balance::Unavailable("credential unreadable".into()),
        };
        rows.push(Row {
            account: id.to_string(),
            kind: format!("{brand} id"),
            active: active == Some(id),
            balance,
        });
    }
    rows
}

/// The stored provider keys as rows — opaque (no Hanzo ledger to read), so they
/// carry the provider label and never a number. Provider keys are GLOBAL (not
/// brand-scoped), so they show only under the default brand — mirroring how
/// `logout --all` clears them for the default brand alone.
fn provider_rows(brand: &str) -> Result<Vec<Row>> {
    if brand != paths::DEFAULT_BRAND {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for p in [Provider::Hanzo, Provider::OpenAI, Provider::Anthropic] {
        if provider::key(p)?.is_some() {
            rows.push(Row {
                account: p.slug().to_string(),
                kind: "provider key".to_string(),
                // `*` marks the active IDENTITY only — the billing principal, "who
                // am I". A provider key is a routing credential on a different
                // axis, so it never carries the active marker.
                active: false,
                balance: Balance::ProviderKey,
            });
        }
    }
    Ok(rows)
}

/// Classify one balance response into a row state — PURE, so the disjoint error
/// path (a 401 or a network fault on ONE account never blanks the others) is a
/// test, not a hope. `http::send` hands back a non-2xx status rather than an
/// `Err`, so a refusal is `Ok((4xx, _))`; only a transport fault is `Err`.
fn classify(result: Result<(StatusCode, Value)>) -> Balance {
    let (status, body) = match result {
        Ok(pair) => pair,
        // A transport fault on ONE account is that account's problem alone.
        Err(_) => return Balance::Unavailable("network unreachable".into()),
    };
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        // This account's token is expired / rejected — re-auth just this one.
        return Balance::Unavailable("sign in again".into());
    }
    if !status.is_success() {
        return Balance::Unavailable(format!("upstream {}", status.as_u16()));
    }
    match read_cents(&body) {
        Some((available, balance, holds)) => Balance::Read { available, balance, holds },
        // A 2xx with no amount is UNKNOWN, never a rendered zero.
        None => Balance::Unavailable("unreadable".into()),
    }
}

/// `(available, balance, holds)` from commerce's `{balance,holds,available}` cents
/// wire. `available` is the spendable "usage left"; it falls back to `balance`
/// when only that is present. `None` when the body carries NO amount at all — an
/// unreadable balance is unknown, never a zero (the same rule `hanzo billing`
/// enforces). Amounts are read as integers (Go `int64` → JSON integer), so money
/// is exact — a float body reads as unreadable rather than as a rounded number.
fn read_cents(v: &Value) -> Option<(i64, i64, i64)> {
    let get = |k: &str| v.get(k).and_then(Value::as_i64);
    let available = get("available").or_else(|| get("balance"))?;
    let balance = get("balance").unwrap_or(available);
    let holds = get("holds").unwrap_or(0);
    Some((available, balance, holds))
}

/// Render the stacked view — PURE over the rows, so "one failed account never
/// blanks the others" holds by construction: N rows in, N lines out.
fn render(rows: &[Row]) -> String {
    let acct_w = rows.iter().map(|r| r.account.len()).max().unwrap_or(0).max(7);
    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(0).max(4);
    rows.iter()
        .map(|r| {
            let mark = if r.active { "*".green().to_string() } else { " ".to_string() };
            // Pad THEN color, so the ANSI codes never count toward column width.
            let acct = format!("{:<acct_w$}", r.account);
            let acct = if r.active { acct.bold().to_string() } else { acct };
            let kind = format!("{:<kind_w$}", r.kind).dimmed();
            format!("  {mark} {acct}  {kind}  {}", cell(&r.balance))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The right-hand cell: the money (usage left) or an honest status word.
fn cell(b: &Balance) -> String {
    match b {
        Balance::Read { available, balance, holds } => {
            let left = usd(*available).green().bold().to_string();
            // Surface a settled balance / holds ONLY when they add information;
            // co-resident they are equal / zero, so the common line stays clean.
            if *holds != 0 || balance != available {
                let detail = format!("(balance {} · holds {})", usd(*balance), usd(*holds));
                format!("{left}  {}", detail.dimmed())
            } else {
                left
            }
        }
        Balance::ProviderKey => "direct route — not metered here".dimmed().to_string(),
        Balance::NoCredential => "no credential — run `hanzo login`".yellow().to_string(),
        Balance::Unavailable(why) => {
            format!("{} {}", "unavailable".yellow(), format!("({why})").dimmed())
        }
    }
}

/// Render integer USD cents as real currency — EXACT, integer-only (no float, so
/// no rounding error on money). The balance wire is always USD cents, so the
/// exponent is known (2); this is real currency, not a guessed decimal point.
fn usd(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs(); // u64 — correct even for i64::MIN
    format!("{sign}${}.{:02}", group_thousands(abs / 100), abs % 100)
}

/// `1234567` → `"1,234,567"`. Stdlib only — no dependency for a six-line group.
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::code::testmock::MockCloud;
    use serde_json::json;

    fn id(s: &str) -> Identity {
        // Derived from claims, as everywhere else — the only way to build one.
        let (owner, name) = s.split_once('/').unwrap();
        Identity::from_access_token(&crate::iam::identity::testjwt::jwt(owner, name)).unwrap()
    }

    // ---- money is rendered as EXACT real currency --------------------------

    #[test]
    fn usd_renders_exact_currency_from_cents() {
        assert_eq!(usd(125_000), "$1,250.00");
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(5), "$0.05");
        assert_eq!(usd(99), "$0.99");
        assert_eq!(usd(100), "$1.00");
        assert_eq!(usd(1_234_567), "$12,345.67");
        assert_eq!(usd(-350), "-$3.50");
        // No float anywhere — a large exact cents value renders exactly.
        assert_eq!(usd(100_000_000), "$1,000,000.00");
    }

    // ---- reading the wire honestly -----------------------------------------

    #[test]
    fn read_cents_prefers_available_and_never_zeroes_the_unknown() {
        assert_eq!(read_cents(&json!({"available":10,"balance":20,"holds":5})), Some((10, 20, 5)));
        // available absent → falls back to balance; holds defaults to 0.
        assert_eq!(read_cents(&json!({"balance":20})), Some((20, 20, 0)));
        // No amount at all → None (unknown), NOT a zero the server never sent.
        assert_eq!(read_cents(&json!({"currency":"usd"})), None);
        // A float body is treated as unreadable rather than rounded.
        assert_eq!(read_cents(&json!({"available":10.5})), None);
    }

    #[test]
    fn classify_maps_each_outcome_honestly() {
        // 200 + the cents wire → Read (usage left = available).
        let ok = classify(Ok((
            StatusCode::OK,
            json!({"balance":125000,"holds":0,"available":125000}),
        )));
        assert_eq!(ok, Balance::Read { available: 125_000, balance: 125_000, holds: 0 });
        // 401 / 403 → a sign-in prompt, never a zero.
        assert!(matches!(
            classify(Ok((StatusCode::UNAUTHORIZED, Value::Null))),
            Balance::Unavailable(_)
        ));
        assert!(matches!(
            classify(Ok((StatusCode::FORBIDDEN, Value::Null))),
            Balance::Unavailable(_)
        ));
        // 5xx → unavailable with the code.
        assert!(matches!(
            classify(Ok((StatusCode::BAD_GATEWAY, Value::Null))),
            Balance::Unavailable(_)
        ));
        // 200 with no amount → unreadable (unknown), never a rendered zero.
        assert!(matches!(
            classify(Ok((StatusCode::OK, json!({"nope":1})))),
            Balance::Unavailable(_)
        ));
        // A transport fault → network, isolated to this account.
        assert!(matches!(
            classify(Err(anyhow::anyhow!("connection refused"))),
            Balance::Unavailable(_)
        ));
    }

    // ---- THE disjoint property: one failure never blanks the others --------

    #[test]
    fn render_shows_every_row_even_when_some_failed() {
        let rows = vec![
            Row {
                account: "hanzo/z".into(),
                kind: "hanzo id".into(),
                active: true,
                balance: Balance::Read { available: 125_000, balance: 125_000, holds: 0 },
            },
            Row {
                account: "admin/z".into(),
                kind: "hanzo id".into(),
                active: false,
                balance: Balance::Unavailable("sign in again".into()),
            },
            Row {
                account: "openai".into(),
                kind: "provider key".into(),
                active: false,
                balance: Balance::ProviderKey,
            },
            Row {
                account: "lux/z".into(),
                kind: "lux id".into(),
                active: false,
                balance: Balance::NoCredential,
            },
        ];
        let out = render(&rows);
        // The funded account shows its real balance ...
        assert!(out.contains("hanzo/z"), "{out}");
        assert!(out.contains("$1,250.00"), "the funded account shows real currency: {out}");
        // ... and the FAILED account still renders, never blanking the view.
        assert!(out.contains("admin/z") && out.contains("unavailable"), "failed row present: {out}");
        assert!(out.contains("openai") && out.contains("provider key"), "provider row present: {out}");
        assert!(out.contains("lux/z") && out.contains("credential"), "no-cred row present: {out}");
        // Four accounts in, four lines out — nothing dropped.
        assert_eq!(out.lines().count(), 4, "every row renders: {out}");
    }

    #[test]
    fn render_surfaces_holds_only_when_they_add_information() {
        let clean = render(&[Row {
            account: "a".into(),
            kind: "hanzo id".into(),
            active: false,
            balance: Balance::Read { available: 100, balance: 100, holds: 0 },
        }]);
        assert!(clean.contains("$1.00"));
        assert!(!clean.contains("holds"), "no holds line when holds==0 and balance==available");

        let held = render(&[Row {
            account: "a".into(),
            kind: "hanzo id".into(),
            active: false,
            balance: Balance::Read { available: 100, balance: 150, holds: 50 },
        }]);
        assert!(held.contains("holds"), "holds surfaced when non-zero: {held}");
        assert!(held.contains("$1.50") && held.contains("$0.50"), "{held}");
    }

    #[test]
    fn a_provider_key_row_never_shows_a_fabricated_number() {
        let out = render(&[Row {
            account: "anthropic".into(),
            kind: "provider key".into(),
            active: false,
            balance: Balance::ProviderKey,
        }]);
        assert!(out.contains("anthropic") && out.contains("provider key"));
        // No dollar amount is ever invented for an opaque key.
        assert!(!out.contains('$'), "must not fabricate a balance for a provider key: {out}");
    }

    // ---- the fan-out, end-to-end against a mock cloud ----------------------

    /// Every account is fetched INDEPENDENTLY with its OWN bearer, the active one
    /// is marked, and the CLI never sends an org.
    #[tokio::test]
    async fn fetch_rows_fans_out_per_account_with_each_own_token() {
        let mock = MockCloud::start().await;
        let client = Client::new();
        let org = id("hanzo/z");
        let admin = id("admin/z");
        let accounts = vec![
            (org.clone(), Cred::Token("ORG_TOK".to_string())),
            (admin.clone(), Cred::Token("ADMIN_TOK".to_string())),
        ];

        let rows = fetch_rows(&client, &mock.base_url(), "hanzo", Some(&admin), &accounts).await;

        assert_eq!(rows.len(), 2);
        // Both read their OWN balance (the mock answers 125000 cents = $1,250.00).
        assert_eq!(rows[0].balance, Balance::Read { available: 125_000, balance: 125_000, holds: 0 });
        assert_eq!(rows[1].balance, Balance::Read { available: 125_000, balance: 125_000, holds: 0 });
        assert!(!rows[0].active, "hanzo/z is not active here");
        assert!(rows[1].active, "admin/z is the active identity, marked");

        // Each account was fetched with ITS OWN bearer, at the right path, no org.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "one request per account — a true fan-out");
        for r in &reqs {
            assert_eq!(r.method, "GET");
            assert_eq!(r.path, BALANCE_PATH);
            assert!(r.header("x-org-id").is_none(), "CLI must never send an org header");
        }
        let bearers: Vec<_> = reqs.iter().filter_map(|r| r.header("authorization")).collect();
        assert!(bearers.contains(&"Bearer ORG_TOK".to_string()), "org read with its own token");
        assert!(bearers.contains(&"Bearer ADMIN_TOK".to_string()), "admin read with its own token");
    }

    /// A held-but-credential-missing identity is NOT fetched and never fabricates
    /// a balance — and the funded account beside it still renders.
    #[tokio::test]
    async fn a_no_credential_account_is_not_fetched_and_does_not_blank_the_rest() {
        let mock = MockCloud::start().await;
        let client = Client::new();
        let accounts = vec![
            (id("hanzo/z"), Cred::Token("ORG_TOK".to_string())),
            (id("admin/z"), Cred::Missing), // credential revoked / wiped
        ];

        let rows = fetch_rows(&client, &mock.base_url(), "hanzo", Some(&id("hanzo/z")), &accounts).await;

        assert!(matches!(rows[0].balance, Balance::Read { .. }), "funded account reads");
        assert_eq!(rows[1].balance, Balance::NoCredential, "missing credential is honest");
        // Only the account WITH a credential hit the wire.
        assert_eq!(mock.requests().len(), 1, "the ghost account triggered no request");
    }

    /// THE LOW-1 FIX: a corrupt / mismatched NON-active slot (its `token_for`
    /// errored) folds into its OWN `Unavailable` row — it never aborts the whole
    /// view. The funded account beside it still renders. Disjoint at RESOLUTION,
    /// not just at fetch.
    #[tokio::test]
    async fn an_unreadable_credential_is_isolated_and_does_not_blank_the_view() {
        let mock = MockCloud::start().await;
        let client = Client::new();
        let accounts = vec![
            (id("hanzo/z"), Cred::Token("ORG_TOK".to_string())),
            (id("admin/z"), Cred::Unreadable), // corrupt / mismatched non-active slot
        ];

        let rows = fetch_rows(&client, &mock.base_url(), "hanzo", Some(&id("hanzo/z")), &accounts).await;

        assert!(matches!(rows[0].balance, Balance::Read { .. }), "the funded account still renders");
        assert!(
            matches!(&rows[1].balance, Balance::Unavailable(_)),
            "an unreadable slot is its OWN row, not a whole-view abort"
        );
        assert_eq!(mock.requests().len(), 1, "the unreadable account triggered no fetch");
    }

    /// A 401 on an account is isolated to its row — the command still produces a
    /// row and never bails.
    #[tokio::test]
    async fn a_401_is_isolated_to_its_row() {
        let mock = MockCloud::start_status(401).await;
        let client = Client::new();
        let accounts = vec![(id("hanzo/z"), Cred::Token("EXPIRED".to_string()))];

        let rows = fetch_rows(&client, &mock.base_url(), "hanzo", None, &accounts).await;

        assert_eq!(rows.len(), 1, "the fan-out completed despite the 401");
        assert!(matches!(&rows[0].balance, Balance::Unavailable(_)), "401 → unavailable, not a zero");
    }
}
