//! RFC 8628 Device Authorization Grant — the headless-safe login path.
//!
//! When there is no browser to open (a GPU box, an ssh session, CI, or an
//! explicit `--device`), the CLI asks IAM for a device+user code, shows the
//! verification link as text AND a scannable terminal QR, and polls the token
//! endpoint until the user approves in any signed-in browser. No password ever
//! touches this terminal. Endpoints come from [`super::paths`] (brand-aware via
//! `server_url_for_brand`); the device grant runs as the `<brand>-app` client —
//! the one IAM seeds with the `device_code` grant enabled.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::oauth::{self, SCOPE};
use super::paths::{self, DEVICE, TOKEN};
use super::token::TokenSet;

/// The RFC 8628 device-grant type sent on the token poll.
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The device-flow client. It is `hanzo-cli` — the SAME registration the browser
/// flow uses, because IAM enables both grants on it and measuring said so:
/// `hanzo-cli` mints a device_code, and `hanzo-app` answers `invalid_client`.
/// One client for one CLI; the flow it happens to be running is not an identity.
fn device_client_id(_brand: &str) -> &'static str {
    oauth::CLIENT_ID
}

/// The RFC 8628 device authorization response.
#[derive(Debug, Default, Deserialize)]
pub struct DeviceAuthResp {
    #[serde(default)]
    pub device_code: String,
    #[serde(default)]
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub interval: i64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

/// A raw token-endpoint response during the poll — carries EITHER the token
/// fields OR an RFC 8628 error (`authorization_pending`/`slow_down`/…). It is
/// converted to a [`TokenSet`] only once a token actually issues.
#[derive(Debug, Default, Deserialize)]
struct DeviceTokenResp {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

impl DeviceTokenResp {
    fn into_token_set(self) -> TokenSet {
        TokenSet {
            access_token: self.access_token,
            token_type: if self.token_type.is_empty() {
                "Bearer".to_string()
            } else {
                self.token_type
            },
            refresh_token: self.refresh_token,
            id_token: self.id_token,
            expires_in: self.expires_in,
            scope: self.scope,
        }
    }
}

/// Start the device flow: mint a device_code + user_code pair for `brand`.
pub async fn device_auth(origin: &str, client_id: &str, scope: &str) -> Result<DeviceAuthResp> {
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, DEVICE))
        .query(&[
            ("client_id", client_id),
            ("scope", scope),
            ("response_type", "device_code"),
        ])
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    let da: DeviceAuthResp = serde_json::from_str(&body)
        .map_err(|_| anyhow!("iam device auth: HTTP {}: {}", status, body.trim()))?;
    if !da.error.is_empty() {
        return Err(anyhow!(
            "iam device auth: {}: {}",
            da.error,
            da.error_description
        ));
    }
    if da.device_code.is_empty() || da.user_code.is_empty() {
        return Err(anyhow!(
            "iam device auth: HTTP {}: no device_code in response",
            status
        ));
    }
    Ok(da)
}

/// Post the token endpoint once, returning the raw response WITHOUT mapping
/// OAuth errors — the poll loop must inspect authorization_pending/slow_down.
async fn request_token(
    origin: &str,
    client_id: &str,
    device_code: &str,
) -> Result<DeviceTokenResp> {
    let resp = reqwest::Client::new()
        .post(paths::iam_url(origin, TOKEN))
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", DEVICE_GRANT_TYPE),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    serde_json::from_str(&body)
        .map_err(|_| anyhow!("iam device token: HTTP {}: {}", status, body.trim()))
}

/// The decision for one token-endpoint response, decoupled from timing so the
/// state machine is unit-testable without a clock or a server.
#[derive(Debug)]
enum Poll {
    Done(TokenSet),
    Pending,
    SlowDown,
    Fail(String),
}

/// Map one token response to a poll decision per RFC 8628 §3.5.
fn classify(tr: DeviceTokenResp) -> Poll {
    match tr.error.clone().as_str() {
        "" => {
            if tr.access_token.is_empty() {
                Poll::Fail("iam device token: empty access_token".into())
            } else {
                Poll::Done(tr.into_token_set())
            }
        }
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "expired_token" => {
            Poll::Fail("device code expired before approval — run `hanzo login` again".into())
        }
        other => Poll::Fail(format!(
            "iam device token: {}: {}",
            other, tr.error_description
        )),
    }
}

/// Drive the token poll: sleep, POST, decide — until issued, expired, or
/// denied. `interval`/`deadline` are passed in so tests can run instantly.
async fn poll_loop(
    origin: &str,
    client_id: &str,
    device_code: &str,
    mut interval: Duration,
    deadline: Instant,
) -> Result<TokenSet> {
    loop {
        tokio::time::sleep(interval).await;
        if Instant::now() > deadline {
            return Err(anyhow!(
                "device code expired before approval — run `hanzo login` again"
            ));
        }
        match classify(request_token(origin, client_id, device_code).await?) {
            Poll::Done(tokens) => return Ok(tokens),
            Poll::Pending => {}
            Poll::SlowDown => interval += Duration::from_secs(5),
            Poll::Fail(msg) => return Err(anyhow!(msg)),
        }
    }
}

/// Poll until the user approves or the code expires. The interval floors at 1s
/// per RFC 8628; slow_down backs off inside the loop.
async fn poll_device_token(origin: &str, client_id: &str, da: &DeviceAuthResp) -> Result<TokenSet> {
    let interval = Duration::from_secs(da.interval.max(1) as u64);
    let deadline = Instant::now() + Duration::from_secs(da.expires_in.max(1) as u64);
    poll_loop(origin, client_id, &da.device_code, interval, deadline).await
}

/// Run the full interactive device sign-in for `brand` and return the tokens.
pub async fn login(brand: &str) -> Result<TokenSet> {
    let origin = oauth::server_url(brand)?;
    let client_id = device_client_id(brand);
    let da = device_auth(origin, client_id, SCOPE).await?;

    let link = if da.verification_uri_complete.is_empty() {
        &da.verification_uri
    } else {
        &da.verification_uri_complete
    };
    println!("\nSign in to {brand} on any device:\n");
    print_qr(link);
    println!("\n  {link}");
    if !da.verification_uri.is_empty() && !da.user_code.is_empty() {
        println!(
            "  or open {} and enter code {}",
            da.verification_uri, da.user_code
        );
    }
    println!("\nWaiting for approval…");
    poll_device_token(origin, client_id, &da).await
}

/// Render a scannable QR of the verification link; skip silently if it cannot
/// be encoded (never block sign-in on the QR).
fn print_qr(link: &str) {
    if let Ok(qr) = qr2term::generate_qr_string(link) {
        print!("{qr}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    // A single-threaded stub of the two IAM endpoints. Each incoming connection
    // gets the next scripted JSON body; `Connection: close` makes reqwest open a
    // fresh connection per poll, so one connection == one scripted response.
    fn spawn_mock(responses: Vec<serde_json::Value>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (i, stream) in listener.incoming().enumerate() {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = responses
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}))
                    .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                if i + 1 >= responses.len() {
                    break;
                }
            }
        });
        url
    }

    #[test]
    fn the_device_client_is_the_cli_itself() {
        // Measured against the live endpoint, not assumed: `hanzo-cli` mints a
        // device_code and `hanzo-app` answers invalid_client. One CLI, one
        // registration, whichever flow it is running.
        assert_eq!(device_client_id("hanzo"), oauth::CLIENT_ID);
        assert_eq!(device_client_id("lux"), oauth::CLIENT_ID);
        assert_eq!(device_client_id("zoo"), oauth::CLIENT_ID);
    }

    #[test]
    fn classify_covers_every_branch() {
        assert!(matches!(
            classify(DeviceTokenResp {
                access_token: "t".into(),
                ..Default::default()
            }),
            Poll::Done(_)
        ));
        assert!(matches!(
            classify(DeviceTokenResp {
                error: "authorization_pending".into(),
                ..Default::default()
            }),
            Poll::Pending
        ));
        assert!(matches!(
            classify(DeviceTokenResp {
                error: "slow_down".into(),
                ..Default::default()
            }),
            Poll::SlowDown
        ));
        assert!(matches!(
            classify(DeviceTokenResp {
                error: "expired_token".into(),
                ..Default::default()
            }),
            Poll::Fail(_)
        ));
        match classify(DeviceTokenResp {
            error: "access_denied".into(),
            error_description: "user refused".into(),
            ..Default::default()
        }) {
            Poll::Fail(m) => assert!(m.contains("access_denied")),
            other => panic!("{other:?}"),
        }
        // Success shape with no token is a failure, not a false "Done".
        assert!(matches!(
            classify(DeviceTokenResp::default()),
            Poll::Fail(_)
        ));
    }

    #[test]
    fn done_maps_into_token_set_with_default_type() {
        match classify(DeviceTokenResp {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            expires_in: Some(3600),
            ..Default::default()
        }) {
            Poll::Done(ts) => {
                assert_eq!(ts.access_token, "AT");
                assert_eq!(ts.token_type, "Bearer"); // defaulted
                assert_eq!(ts.refresh_token.as_deref(), Some("RT"));
                assert_eq!(ts.expires_in, Some(3600));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn device_auth_parses_response() {
        let url = spawn_mock(vec![serde_json::json!({
            "device_code": "dc-1",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://iam.hanzo.ai/device",
            "verification_uri_complete": "https://iam.hanzo.ai/device/WDJB-MJHT",
            "expires_in": 900,
            "interval": 1,
        })]);
        let da = device_auth(&url, "hanzo-app", SCOPE).await.unwrap();
        assert_eq!(da.device_code, "dc-1");
        assert_eq!(da.user_code, "WDJB-MJHT");
        assert!(da.verification_uri_complete.ends_with("/WDJB-MJHT"));
    }

    #[tokio::test]
    async fn device_auth_surfaces_error() {
        let url = spawn_mock(vec![serde_json::json!({
            "error": "invalid_client", "error_description": "unknown client"
        })]);
        let err = device_auth(&url, "hanzo-app", SCOPE)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid_client"), "{err}");
    }

    #[tokio::test]
    async fn poll_pending_then_issued() {
        let url = spawn_mock(vec![
            serde_json::json!({"error": "authorization_pending"}),
            serde_json::json!({"error": "authorization_pending"}),
            serde_json::json!({"access_token": "tok-1", "token_type": "Bearer", "expires_in": 3600}),
        ]);
        let ts = poll_loop(
            &url,
            "hanzo-app",
            "dc-1",
            Duration::from_millis(5),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(ts.access_token, "tok-1");
    }

    #[tokio::test]
    async fn poll_denied_terminates() {
        let url = spawn_mock(vec![serde_json::json!({
            "error": "access_denied", "error_description": "user refused"
        })]);
        let err = poll_loop(
            &url,
            "hanzo-app",
            "dc-1",
            Duration::from_millis(5),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("access_denied"), "{err}");
    }

    #[tokio::test]
    async fn poll_deadline_expires() {
        let url = spawn_mock(vec![serde_json::json!({"error": "authorization_pending"})]);
        let err = poll_loop(
            &url,
            "hanzo-app",
            "dc-1",
            Duration::from_millis(1),
            Instant::now(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("expired"), "{err}");
    }
}
