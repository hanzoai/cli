// device.rs — RFC 8628 Device Authorization Grant against Hanzo IAM: the ONE
// way any machine signs in. Ask IAM for a device+user code, then poll the token
// endpoint until the user approves in any browser. No password touches this
// terminal; works headless (GPU boxes, ssh, CI). Mirrors the Go CLI's
// device.go so both share one server-side client config.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};

pub const IAM_ISSUER: &str = "https://hanzo.id";
pub const CLIENT_ID: &str = "hanzo-app";
pub const SCOPE: &str = "openid profile email";
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

const USER_AGENT: &str = concat!("hanzo-cli/", env!("CARGO_PKG_VERSION"));

/// deviceAuthResp is the RFC 8628 device authorization response.
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

/// tokenResp is the OAuth2 token endpoint response (success or RFC-6749 error).
#[derive(Debug, Default, Deserialize)]
pub struct TokenResp {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

/// Iam is a thin client over the Hanzo IAM OAuth2 surface at
/// {issuer}/v1/iam/oauth/*. It holds no IAM business logic.
pub struct Iam {
    base: String,
    client_id: String,
    http: reqwest::Client,
}

impl Iam {
    pub fn new(issuer: &str, client_id: &str) -> Self {
        Self {
            base: issuer.trim_end_matches('/').to_string(),
            client_id: client_id.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(USER_AGENT)
                .build()
                .expect("build reqwest client"),
        }
    }

    /// Start the device flow: mint a device_code + user_code pair.
    pub async fn device_auth(&self, scope: &str) -> Result<DeviceAuthResp> {
        let resp = self
            .http
            .post(format!("{}/v1/iam/oauth/device", self.base))
            .query(&[
                ("client_id", self.client_id.as_str()),
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
    pub async fn request_token(&self, device_code: &str) -> Result<TokenResp> {
        let resp = self
            .http
            .post(format!("{}/v1/iam/oauth/token", self.base))
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", DEVICE_GRANT_TYPE),
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code),
            ])
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        serde_json::from_str(&body)
            .map_err(|_| anyhow!("iam device token: HTTP {}: {}", status, body.trim()))
    }
}

/// Poll is the decision for one token-endpoint response, decoupled from timing
/// so the state machine is unit-testable without a clock or a server.
#[derive(Debug)]
pub enum Poll {
    Done(TokenResp),
    Pending,
    SlowDown,
    Fail(String),
}

/// classify maps one token response to a poll decision per RFC 8628 §3.5.
pub fn classify(tr: TokenResp) -> Poll {
    match tr.error.clone().as_str() {
        "" => {
            if tr.access_token.is_empty() {
                Poll::Fail("iam device token: empty access_token".into())
            } else {
                Poll::Done(tr)
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

/// poll_loop drives the token poll: sleep, POST, decide — until issued, expired,
/// or denied. `interval`/`deadline` are passed in so tests can run instantly.
async fn poll_loop(
    iam: &Iam,
    device_code: &str,
    mut interval: Duration,
    deadline: Instant,
) -> Result<TokenResp> {
    loop {
        tokio::time::sleep(interval).await;
        if Instant::now() > deadline {
            return Err(anyhow!(
                "device code expired before approval — run `hanzo login` again"
            ));
        }
        match classify(iam.request_token(device_code).await?) {
            Poll::Done(tr) => return Ok(tr),
            Poll::Pending => {}
            Poll::SlowDown => interval += Duration::from_secs(5),
            Poll::Fail(msg) => return Err(anyhow!(msg)),
        }
    }
}

/// poll_device_token polls until the user approves or the code expires. The
/// interval floors at 1s per RFC 8628; slow_down backs off inside the loop.
pub async fn poll_device_token(iam: &Iam, da: &DeviceAuthResp) -> Result<TokenResp> {
    let interval = Duration::from_secs(da.interval.max(1) as u64);
    let deadline = Instant::now() + Duration::from_secs(da.expires_in.max(1) as u64);
    poll_loop(iam, &da.device_code, interval, deadline).await
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
    fn classify_covers_every_branch() {
        assert!(matches!(
            classify(TokenResp {
                access_token: "t".into(),
                ..Default::default()
            }),
            Poll::Done(_)
        ));
        assert!(matches!(
            classify(TokenResp {
                error: "authorization_pending".into(),
                ..Default::default()
            }),
            Poll::Pending
        ));
        assert!(matches!(
            classify(TokenResp {
                error: "slow_down".into(),
                ..Default::default()
            }),
            Poll::SlowDown
        ));
        assert!(matches!(
            classify(TokenResp {
                error: "expired_token".into(),
                ..Default::default()
            }),
            Poll::Fail(_)
        ));
        // access_denied and unknown errors both terminate.
        match classify(TokenResp {
            error: "access_denied".into(),
            error_description: "user refused".into(),
            ..Default::default()
        }) {
            Poll::Fail(m) => assert!(m.contains("access_denied")),
            other => panic!("{other:?}"),
        }
        // Success shape with no token is a failure, not a false "Done".
        assert!(matches!(classify(TokenResp::default()), Poll::Fail(_)));
    }

    #[tokio::test]
    async fn device_auth_parses_response() {
        let url = spawn_mock(vec![serde_json::json!({
            "device_code": "dc-1",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://hanzo.id/device",
            "verification_uri_complete": "https://hanzo.id/device/WDJB-MJHT",
            "expires_in": 900,
            "interval": 1,
        })]);
        let iam = Iam::new(&url, CLIENT_ID);
        let da = iam.device_auth(SCOPE).await.unwrap();
        assert_eq!(da.device_code, "dc-1");
        assert_eq!(da.user_code, "WDJB-MJHT");
        assert!(da.verification_uri_complete.ends_with("/WDJB-MJHT"));
    }

    #[tokio::test]
    async fn device_auth_surfaces_error() {
        let url = spawn_mock(vec![serde_json::json!({
            "error": "invalid_client", "error_description": "unknown client"
        })]);
        let iam = Iam::new(&url, CLIENT_ID);
        let err = iam.device_auth(SCOPE).await.unwrap_err().to_string();
        assert!(err.contains("invalid_client"), "{err}");
    }

    #[tokio::test]
    async fn poll_pending_then_issued() {
        let url = spawn_mock(vec![
            serde_json::json!({"error": "authorization_pending"}),
            serde_json::json!({"error": "authorization_pending"}),
            serde_json::json!({"access_token": "tok-1", "token_type": "Bearer", "expires_in": 3600}),
        ]);
        let iam = Iam::new(&url, CLIENT_ID);
        // Fast interval so the loop test does not sleep for real seconds.
        let tr = poll_loop(
            &iam,
            "dc-1",
            Duration::from_millis(5),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(tr.access_token, "tok-1");
    }

    #[tokio::test]
    async fn poll_denied_terminates() {
        let url = spawn_mock(vec![serde_json::json!({
            "error": "access_denied", "error_description": "user refused"
        })]);
        let iam = Iam::new(&url, CLIENT_ID);
        let err = poll_loop(
            &iam,
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
        // Server that always says pending; deadline is already in the past.
        let url = spawn_mock(vec![serde_json::json!({"error": "authorization_pending"})]);
        let iam = Iam::new(&url, CLIENT_ID);
        let err = poll_loop(&iam, "dc-1", Duration::from_millis(1), Instant::now())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("expired"), "{err}");
    }
}
