use anyhow::{Context, Result};
use colored::*;
use reqwest::Client;
use serde_json::{json, Value};

// Cluster operations against a Hanzo node's v2 API (/v1/node/cluster/*). The node must be
// running with HANZO_CLUSTER_MODE=1. Responses are gzip-compressed by the node, so the
// reqwest `gzip` feature must be enabled (see Cargo.toml).

fn client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("failed to build http client")
}

fn print_json(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

async fn get_q(node: &str, path: &str, query: &[(&str, &str)]) -> Result<Value> {
    let url = format!("{}{}", node.trim_end_matches('/'), path);
    let resp = client()?
        .get(&url)
        .query(query)
        .send()
        .await
        .context("request failed — is the node running?")?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .context("failed to parse node response as JSON")?;
    if !status.is_success() {
        anyhow::bail!("node returned {}: {}", status, body);
    }
    Ok(body)
}

async fn post(node: &str, path: &str, payload: &Value) -> Result<Value> {
    let url = format!("{}{}", node.trim_end_matches('/'), path);
    let resp = client()?
        .post(&url)
        .json(payload)
        .send()
        .await
        .context("request failed — is the node running?")?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .context("failed to parse node response as JSON")?;
    if !status.is_success() {
        anyhow::bail!("node returned {}: {}", status, body);
    }
    Ok(body)
}

pub async fn topology(node: String) -> Result<()> {
    print_json(&get_q(&node, "/v1/node/cluster/topology", &[]).await?);
    Ok(())
}

pub async fn models(node: String) -> Result<()> {
    print_json(&get_q(&node, "/v1/node/cluster/models", &[]).await?);
    Ok(())
}

pub async fn route(node: String, model: String) -> Result<()> {
    print_json(&get_q(&node, "/v1/node/cluster/route", &[("model", &model)]).await?);
    Ok(())
}

pub async fn placement(node: String, model: String) -> Result<()> {
    print_json(&get_q(&node, "/v1/node/cluster/placement", &[("model", &model)]).await?);
    Ok(())
}

pub async fn chat(node: String, model: String, message: String, max_tokens: u32) -> Result<()> {
    let payload = json!({
        "model": model,
        "messages": [{ "role": "user", "content": message }],
        "max_tokens": max_tokens,
        "stream": false,
    });
    let v = post(&node, "/v1/node/cluster/chat", &payload).await?;

    // Pretty path: show who served it + the assistant content.
    let content = v
        .get("response")
        .and_then(|r| r.get("choices"))
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str());
    match content {
        Some(text) => {
            let served = v.get("served_by").and_then(|s| s.as_str()).unwrap_or("?");
            let who = v
                .get("peer")
                .and_then(|p| p.get("node_name"))
                .and_then(|n| n.as_str())
                .or_else(|| v.get("node_name").and_then(|n| n.as_str()))
                .unwrap_or("");
            println!("{} {}", format!("[{} {}]", served, who).dimmed(), text);
        }
        None => print_json(&v),
    }
    Ok(())
}

pub async fn search(node: String, query: String, max_results: u32) -> Result<()> {
    let payload = json!({ "query": query, "max_results": max_results });
    print_json(&post(&node, "/v1/node/cluster/search", &payload).await?);
    Ok(())
}
