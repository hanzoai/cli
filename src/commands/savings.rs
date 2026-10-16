//! The cloud's usage meter, read through the CLI's one authenticated seam:
//! `hanzo dev`'s end-of-session block asks it (as Gen's host, `commands::dev`)
//! and so does the console's Savings view. What the answer means — prices,
//! tallies, the meter's rows and how a number is written — is
//! `hanzo_gen::savings`.

use std::time::Duration;

use hanzo_client::Method;
use serde_json::Value;

use crate::commands::product::{envelope_error, Seam};
use crate::config::Config;

/// How long one cloud read may take.
const PATIENCE: Duration = Duration::from_secs(10);

/// `GET /v1/usage/summary` through the one authenticated seam, as its `data`. A
/// refusal, a transport fault and an error envelope are each a reason, never an
/// empty document.
pub async fn summary(cfg: &mut Config, query: &[String]) -> Result<Value, String> {
    let call = async {
        let seam = Seam::open(cfg).await.map_err(|e| format!("{e:#}"))?;
        let reply = seam
            .send(Method::GET, "/v1/usage/summary", query, None)
            .await
            .map_err(|e| format!("{e:#}"))?;
        let body = reply.body;
        if !(200..300).contains(&reply.status) {
            let said = envelope_error(&body)
                .or_else(|| body.get("detail").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| match &body {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                });
            let said: String = said.split_whitespace().collect::<Vec<_>>().join(" ");
            let said: String = said.chars().take(120).collect();
            return Err(format!("{} {said}", reply.status).trim_end().to_string());
        }
        if let Some(msg) = envelope_error(&body) {
            return Err(msg);
        }
        Ok(body.get("data").cloned().unwrap_or(body))
    };
    tokio::time::timeout(PATIENCE, call)
        .await
        .unwrap_or_else(|_| Err(format!("no answer in {}s", PATIENCE.as_secs())))
}
