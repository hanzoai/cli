//! `hanzo cluster` — dedicated cloud clusters (managed Kubernetes via the PaaS
//! DOKS plane, `/v1/paas/cluster/doks/*`).
//!
//! DOKS is per-ORG: the tenant is the active identity's `owner`, ADDRESSED in the
//! path (never a flag), exactly as `kms` — the server re-checks it against the JWT
//! it verifies, so a wrong one 403s you against yourself. `use` selects the
//! default cluster LOCALLY (config); `create`/`list`/`show`/`delete` call cloud
//! through the one authenticated seam.
//!
//! | verb     | wire                                             |
//! |----------|--------------------------------------------------|
//! | `create` | `POST   /v1/paas/cluster/doks/provision`         |
//! | `list`   | `GET    /v1/paas/cluster/doks/fleet`             |
//! | `show`   | `GET    /v1/paas/cluster/doks/{org}/status`      |
//! | `delete` | `DELETE /v1/paas/cluster/doks/{org}`             |

use anyhow::Result;
use colored::*;
use reqwest::Method;
use serde_json::json;

use crate::commands::cloud;
use crate::config::Config;

/// `hanzo cluster create NAME [--region R]` — provision a dedicated cluster.
pub async fn create(cfg: &mut Config, name: String, region: Option<String>) -> Result<()> {
    let mut body = json!({ "name": name });
    if let Some(r) = region {
        body["region"] = json!(r);
    }
    let v = cloud::call(cfg, Method::POST, "/v1/paas/cluster/doks/provision", Some(&body)).await?;
    cloud::print(&v);
    Ok(())
}

/// `hanzo cluster list` — the org's cluster fleet.
pub async fn list(cfg: &mut Config) -> Result<()> {
    let v = cloud::call(cfg, Method::GET, "/v1/paas/cluster/doks/fleet", None).await?;
    cloud::print(&v);
    Ok(())
}

/// `hanzo cluster show [NAME]` — the org's cluster status. DOKS is org-scoped, so
/// the tenant `owner` addresses it; `NAME` is accepted for symmetry (there is one
/// managed cluster per org).
pub async fn show(cfg: &mut Config, _name: String) -> Result<()> {
    let org = cloud::owner(cfg)?;
    let path = format!("/v1/paas/cluster/doks/{}/status", cloud::enc(&org));
    let v = cloud::call(cfg, Method::GET, &path, None).await?;
    cloud::print(&v);
    Ok(())
}

/// `hanzo cluster delete [NAME]` — tear down the org's cluster.
pub async fn delete(cfg: &mut Config, _name: String) -> Result<()> {
    let org = cloud::owner(cfg)?;
    let path = format!("/v1/paas/cluster/doks/{}", cloud::enc(&org));
    let v = cloud::call(cfg, Method::DELETE, &path, None).await?;
    cloud::print(&v);
    Ok(())
}

/// `hanzo cluster use NAME` — select the default cluster, persisted locally (the
/// same non-secret config the network/wallet selection lives in).
pub fn use_cluster(cfg: &mut Config, name: String) -> Result<()> {
    cfg.update(|c| {
        c.cluster.active = Some(name.clone());
        Ok(())
    })?;
    println!("{} default cluster → {}", "✓".green(), name.cyan().bold());
    Ok(())
}
