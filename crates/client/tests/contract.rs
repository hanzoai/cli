//! The contract, proven over a stubbed wire: nothing here touches a network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hanzo_client::{Client, Error, Method, Outcome, Reply, Request, Transport};
use serde_json::{json, Value};

/// A wire that answers from a script and remembers what it was asked.
#[derive(Clone, Default)]
struct Stub {
    seen: Arc<Mutex<Vec<Request>>>,
    script: Arc<Mutex<VecDeque<Reply>>>,
}

impl Stub {
    fn new(script: Vec<Reply>) -> Self {
        Stub {
            seen: Arc::new(Mutex::new(Vec::new())),
            script: Arc::new(Mutex::new(script.into())),
        }
    }

    fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

impl Transport for Stub {
    async fn send(&self, request: Request) -> Result<Reply, Error> {
        self.seen.lock().unwrap().push(request);
        let next = self.script.lock().unwrap().pop_front();
        Ok(next.unwrap_or(Reply { status: 200, body: Value::Null }))
    }
}

fn reply(status: u16, body: Value) -> Reply {
    Reply { status, body }
}

/// A client is shared across tasks, so it must cross a thread boundary — held
/// token and all. Checked at compile time, where the answer cannot drift.
const _: fn() = || {
    fn shareable<T: Send + Sync + 'static>() {}
    shareable::<hanzo_client::Client>();
    shareable::<Client<Stub>>();
};

fn minted(token: &str, seconds: u64) -> Reply {
    reply(200, json!({ "accessToken": token, "expiresIn": seconds }))
}

/// The mint goes to IAM's own host, at IAM's own path, with the target in the
/// `id` query and the operator key as the bearer — and no body.
#[tokio::test]
async fn mint_addresses_iam_and_names_the_subject_in_the_query() {
    let stub = Stub::new(vec![minted("act_1", 3600), reply(200, json!({"ok": true}))]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud
        .r#as("usr_42")
        .call::<Value>(Method::GET, "/v1/card", None)
        .await
        .unwrap()
        .done()
        .unwrap();

    let seen = stub.seen();
    assert_eq!(seen.len(), 2, "one mint, one call");
    assert_eq!(seen[0].url, "https://hanzo.id/v1/iam/tokens/issue?id=usr_42");
    assert_eq!(seen[0].method, Method::POST);
    assert_eq!(seen[0].token.as_deref(), Some("hk-operator"));
    assert!(seen[0].body.is_none(), "IAM reads the grant off the key, not a body");
}

/// The minted token — parsed from camelCase — is what the call rides on. The
/// operator key never leaves the mint.
#[tokio::test]
async fn the_call_rides_the_minted_token_not_the_operator_key() {
    let stub = Stub::new(vec![minted("act_1", 3600), reply(200, json!({"ok": true}))]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud.r#as("usr_42").call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    let seen = stub.seen();
    assert_eq!(seen[1].url, "https://api.hanzo.ai/v1/card");
    assert_eq!(seen[1].token.as_deref(), Some("act_1"));
}

/// An external id carrying reserved characters is encoded exactly once and
/// cannot forge a second parameter.
#[tokio::test]
async fn the_subject_is_encoded_not_interpolated() {
    let stub = Stub::new(vec![minted("act_1", 3600)]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud.r#as("a b&role=admin").call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    assert_eq!(
        stub.seen()[0].url,
        "https://hanzo.id/v1/iam/tokens/issue?id=a+b%26role%3Dadmin"
    );
}

/// A live token is reused: two calls, one mint.
#[tokio::test]
async fn the_token_is_held_until_it_nears_expiry() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(200, json!({"n": 1})),
        reply(200, json!({"n": 2})),
    ]);
    let user = Client::over(stub.clone(), "hk-operator").r#as("usr_42");

    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();
    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    let seen = stub.seen();
    assert_eq!(seen.len(), 3, "mint once, then two calls");
    assert_eq!(seen[1].token.as_deref(), Some("act_1"));
    assert_eq!(seen[2].token.as_deref(), Some("act_1"));
}

/// A token IAM gave no lifetime for is still held — the client assumes one
/// rather than minting per call.
#[tokio::test]
async fn a_lifetime_iam_omitted_is_assumed_not_ignored() {
    let stub = Stub::new(vec![
        reply(200, json!({"accessToken": "act_1"})),
        reply(200, Value::Null),
        reply(200, Value::Null),
    ]);
    let user = Client::over(stub.clone(), "hk-operator").r#as("usr_42");

    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();
    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    assert_eq!(stub.seen().len(), 3, "mint once, then two calls");
}

/// A 401 drops the held token, re-mints and retries ONCE.
#[tokio::test]
async fn a_401_remints_and_retries_once() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(401, json!({"msg": "expired"})),
        minted("act_2", 3600),
        reply(200, json!({"ok": true})),
    ]);
    let user = Client::over(stub.clone(), "hk-operator").r#as("usr_42");

    let out = user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();
    assert_eq!(out.done().unwrap(), json!({"ok": true}));

    let seen = stub.seen();
    assert_eq!(seen.len(), 4, "mint, 401, re-mint, retry");
    assert_eq!(seen[1].token.as_deref(), Some("act_1"));
    assert_eq!(seen[2].url, "https://hanzo.id/v1/iam/tokens/issue?id=usr_42");
    assert_eq!(seen[3].token.as_deref(), Some("act_2"), "the retry rides the NEW token");
}

/// The retry is once, not a loop: a credential that is genuinely rejected
/// surfaces as the server's own 401.
#[tokio::test]
async fn a_second_401_surfaces_rather_than_looping() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(401, json!({"msg": "revoked"})),
        minted("act_2", 3600),
        reply(401, json!({"msg": "revoked"})),
    ]);
    let user = Client::over(stub.clone(), "hk-operator").r#as("usr_42");

    let err = user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap_err();
    assert!(matches!(err, Error::Api { status: 401, .. }));
    assert_eq!(err.to_string(), "401: revoked");
    assert_eq!(stub.seen().len(), 4, "exactly one retry");
}

/// An unscoped client mints nothing — the operator key IS the credential.
#[tokio::test]
async fn an_unscoped_client_never_mints() {
    let stub = Stub::new(vec![reply(200, json!({"ok": true}))]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud.call::<Value>(Method::GET, "/v1/org", None).await.unwrap();

    let seen = stub.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].url, "https://api.hanzo.ai/v1/org");
    assert_eq!(seen[0].token.as_deref(), Some("hk-operator"));
    assert!(cloud.subject().is_none());
}

/// A 202 is HELD, verbatim — and there is no arm to read a value from.
#[tokio::test]
async fn a_202_is_held_and_carries_the_approval() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(
            202,
            json!({"status": "held", "id": "apr_7", "clause": "card.issue", "reason": "over limit"}),
        ),
    ]);
    let user = Client::over(stub, "hk-operator").r#as("usr_42");

    let out = user
        .call::<Value>(Method::POST, "/v1/card", Some(json!({"limit": 500})))
        .await
        .unwrap();

    let Outcome::Held(approval) = &out else { panic!("a 202 must not read as done") };
    assert_eq!(approval.id, "apr_7");
    assert_eq!(approval.clause, "card.issue");
    assert_eq!(approval.reason, "over limit");
    assert_eq!(out.held().unwrap().id, "apr_7");
}

/// Reading a held call's value is refused, and the refusal names the approval.
#[tokio::test]
async fn done_on_a_held_call_is_an_error_naming_the_approval() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(202, json!({"id": "apr_7", "clause": "card.issue", "reason": "over limit"})),
    ]);
    let user = Client::over(stub, "hk-operator").r#as("usr_42");

    let err = user
        .call::<Value>(Method::POST, "/v1/card", None)
        .await
        .unwrap()
        .done()
        .unwrap_err();

    let Error::Held(approval) = &err else { panic!("expected a held error, got {err:?}") };
    assert_eq!(approval.id, "apr_7");
    assert_eq!(err.to_string(), "held for approval apr_7: card.issue");
}

/// `GET /v1/approvals/{id}` answers the same field names, so one shape reads
/// both.
#[tokio::test]
async fn the_polled_approval_reads_with_the_same_shape() {
    let stub = Stub::new(vec![reply(
        200,
        json!({"status": "held", "id": "apr_7", "clause": "card.issue", "reason": "over limit"}),
    )]);
    let cloud = Client::over(stub, "hk-operator");

    let polled: hanzo_client::Approval = cloud
        .call(Method::GET, "/v1/approvals/apr_7", None)
        .await
        .unwrap()
        .done()
        .unwrap();

    assert_eq!(polled.id, "apr_7");
    assert_eq!(polled.clause, "card.issue");
}

/// A field the server omitted reads as empty rather than failing the call.
#[tokio::test]
async fn an_omitted_approval_field_reads_empty() {
    let stub = Stub::new(vec![reply(202, json!({"id": "apr_7"}))]);
    let cloud = Client::over(stub, "hk-operator");

    let out = cloud.call::<Value>(Method::POST, "/v1/card", None).await.unwrap();
    let approval = out.held().unwrap();
    assert_eq!(approval.id, "apr_7");
    assert_eq!(approval.clause, "");
    assert_eq!(approval.reason, "");
}

/// A refusal is typed: the server's status and body, both reachable.
#[tokio::test]
async fn a_refusal_carries_the_status_and_the_body() {
    let stub = Stub::new(vec![reply(
        403,
        json!({"error": {"message": "no grant for that org"}}),
    )]);
    let cloud = Client::over(stub, "hk-operator");

    let err = cloud.call::<Value>(Method::GET, "/v1/org", None).await.unwrap_err();
    let Error::Api { status, body } = &err else { panic!("expected an api error, got {err:?}") };
    assert_eq!(*status, 403);
    assert_eq!(body["error"]["message"], "no grant for that org");
    assert_eq!(err.to_string(), "403: no grant for that org");
}

/// A refusal the mint itself returned is the same typed error — a bad operator
/// key is not disguised as a missing token.
#[tokio::test]
async fn a_refused_mint_is_an_api_error() {
    let stub = Stub::new(vec![reply(401, json!({"msg": "bad key"}))]);
    let user = Client::over(stub, "hk-nope").r#as("usr_42");

    let err = user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap_err();
    assert!(matches!(err, Error::Api { status: 401, .. }));
    assert_eq!(err.to_string(), "401: bad key");
}

/// A mint that answers 200 with no token is named for what it is.
#[tokio::test]
async fn a_tokenless_mint_is_an_auth_error() {
    let stub = Stub::new(vec![reply(200, json!({"expiresIn": 3600}))]);
    let user = Client::over(stub, "hk-operator").r#as("usr_42");

    let err = user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap_err();
    assert!(matches!(&err, Error::Auth { subject } if subject == "usr_42"));
    assert_eq!(err.to_string(), "iam issued no token to act as usr_42");
}

/// The issuer is overridable for a private estate; the platform address is too.
#[tokio::test]
async fn a_private_estate_moves_both_addresses() {
    let stub = Stub::new(vec![minted("act_1", 3600), reply(200, Value::Null)]);
    let user = Client::over(stub.clone(), "hk-operator")
        .issuer("https://id.estate.internal/")
        .base("https://api.estate.internal/")
        .r#as("usr_42");

    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    let seen = stub.seen();
    assert_eq!(seen[0].url, "https://id.estate.internal/v1/iam/tokens/issue?id=usr_42");
    assert_eq!(seen[1].url, "https://api.estate.internal/v1/card");
}

/// Two subjects off one operator credential never share a token.
#[tokio::test]
async fn each_subject_holds_its_own_token() {
    let stub = Stub::new(vec![
        minted("act_a", 3600),
        reply(200, Value::Null),
        minted("act_b", 3600),
        reply(200, Value::Null),
    ]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud.r#as("usr_a").call::<Value>(Method::GET, "/v1/card", None).await.unwrap();
    cloud.r#as("usr_b").call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    let seen = stub.seen();
    assert_eq!(seen[0].url, "https://hanzo.id/v1/iam/tokens/issue?id=usr_a");
    assert_eq!(seen[1].token.as_deref(), Some("act_a"));
    assert_eq!(seen[2].url, "https://hanzo.id/v1/iam/tokens/issue?id=usr_b");
    assert_eq!(seen[3].token.as_deref(), Some("act_b"));
}

/// A clone shares the held token — scoping once is minting once.
#[tokio::test]
async fn a_clone_shares_the_held_token() {
    let stub = Stub::new(vec![
        minted("act_1", 3600),
        reply(200, Value::Null),
        reply(200, Value::Null),
    ]);
    let user = Client::over(stub.clone(), "hk-operator").r#as("usr_42");
    let same = user.clone();

    user.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();
    same.call::<Value>(Method::GET, "/v1/card", None).await.unwrap();

    assert_eq!(stub.seen().len(), 3, "one mint across both handles");
    assert_eq!(same.subject(), Some("usr_42"));
}

/// A body rides as JSON, and the address never grows an `/api/` prefix.
#[tokio::test]
async fn the_body_rides_and_the_address_stays_v1() {
    let stub = Stub::new(vec![reply(200, Value::Null)]);
    let cloud = Client::over(stub.clone(), "hk-operator");

    cloud
        .call::<Value>(Method::POST, "/v1/card", Some(json!({"limit": 500})))
        .await
        .unwrap();

    let seen = stub.seen();
    assert_eq!(seen[0].body, Some(json!({"limit": 500})));
    assert_eq!(seen[0].url, "https://api.hanzo.ai/v1/card");
    assert!(!seen[0].url.contains("/api/"));
}

/// A 2xx body that does not fit the shape asked for is named a decode failure,
/// not silently defaulted.
#[tokio::test]
async fn a_body_that_does_not_fit_is_a_decode_error() {
    #[derive(Debug, serde::Deserialize)]
    struct Card {
        #[allow(dead_code)]
        id: String,
    }

    let stub = Stub::new(vec![reply(200, json!({"nope": 1}))]);
    let cloud = Client::over(stub, "hk-operator");

    let err = cloud.call::<Card>(Method::GET, "/v1/card", None).await.unwrap_err();
    assert!(matches!(err, Error::Decode { status: 200, .. }), "got {err:?}");
}
