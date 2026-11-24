//! The folders of THIS machine the web may start runs in (hanzo-inc/app#310):
//! `hanzo link grant|revoke|folders`, over `/v1/agent/targets/{id}/folders`.
//!
//! A grant lets a run started from the web work in that folder and under it. Only
//! the machine's owner grants or revokes; cloud answers anyone else 403. A folder
//! goes over as its absolute real path, the spelling the machine's runner checks a
//! run's folder against, and a revoke reaches the runner on its next beat.
//!
//! The routes are served (hanzo-inc/cloud f5831678ae) and not yet in the document
//! `spec/cloud.json` pins, so their urls are built from the origin, as `unpublish`
//! does; the next re-pin makes `hanzo agent targets folders` too.

use super::network::url_id;
use super::Caller;
use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use colored::*;
use hanzo_client::{Method, Transport};
use hanzo_gen::context::{self, TargetRecord};
use hanzo_gen::sample::Sampler;
use hanzo_gen::{target, Root};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

/// One active grant.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub folder: String,
    pub granted_by: String,
    pub granted_at: String,
}

#[derive(Debug, Deserialize)]
struct List {
    #[serde(default)]
    folders: Vec<Grant>,
}

#[derive(Debug, Deserialize)]
struct Revoked {
    folder: String,
}

impl Caller {
    /// Grant `folder` on machine `target`.
    pub async fn grant(&self, target: &str, folder: &str) -> Result<Grant> {
        let v = self
            .send(
                Method::POST,
                &folders_path(target)?,
                Some(json!({ "folder": folder })),
            )
            .await?;
        serde_json::from_value(v).context("decode folder grant")
    }

    /// End the grant of `folder` on machine `target`; the folder cloud revoked.
    pub async fn revoke(&self, target: &str, folder: &str) -> Result<String> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}{}", self.api, folders_path(target)?),
            [("folder", folder)],
        )
        .context("building the revoke url")?;
        let v = self
            .http
            .send(self.request(Method::DELETE, url.into()))
            .await?
            .ok()?;
        let r: Revoked = serde_json::from_value(v).context("decode folder revoke")?;
        Ok(r.folder)
    }

    /// The active grants on machine `target`, oldest first.
    pub async fn folders(&self, target: &str) -> Result<Vec<Grant>> {
        let v = self.send(Method::GET, &folders_path(target)?, None).await?;
        let list: List = serde_json::from_value(v).context("decode folder grants")?;
        Ok(list.folders)
    }
}

fn folders_path(target: &str) -> Result<String> {
    if !url_id(target) {
        bail!("{target:?} is not a target id this command will put in a url");
    }
    Ok(format!("/v1/agent/targets/{target}/folders"))
}

/// The absolute real path of a folder that exists: symlinks and `..` resolved.
fn real(folder: &Path) -> Result<PathBuf> {
    let real = std::fs::canonicalize(folder)
        .with_context(|| format!("{} is not a folder here", folder.display()))?;
    if !real.is_dir() {
        bail!("{} is not a folder", real.display());
    }
    Ok(real)
}

/// This machine's target in the caller's org: one heartbeat, which registers it
/// when cloud has none, then the id it answered.
async fn this_machine(caller: &Caller) -> Result<String> {
    let root = Root::need()?;
    let (machine, host) = (context::machine_id(&root), context::hostname(&root));
    let sample = Sampler::new().machine().await;
    target::sync(
        &root,
        &caller.api,
        &caller.token,
        caller.selected(),
        &machine,
        &host,
        &sample,
    )
    .await
    .ok_or_else(|| {
        anyhow!(
            "this machine ({host}) could not be registered with {}",
            caller.api
        )
    })?;
    TargetRecord::load(&root, &machine)?
        .filter(|r| r.host == host && r.api == caller.api)
        .map(|r| r.id)
        .ok_or_else(|| anyhow!("this machine ({host}) has no target"))
}

/// `hanzo link grant FOLDER`.
pub async fn grant(cfg: &mut Config, folder: PathBuf) -> Result<()> {
    let folder = real(&folder)?;
    let caller = Caller::sign_in(cfg, None).await?;
    let id = this_machine(&caller).await?;
    let g = caller.grant(&id, &folder.to_string_lossy()).await?;
    println!(
        "{} the web may start runs in {} on this machine ({id})",
        "✓".green(),
        g.folder.cyan()
    );
    Ok(())
}

/// `hanzo link revoke FOLDER`: a folder no longer on disk is revoked as spelled,
/// made absolute.
pub async fn revoke(cfg: &mut Config, folder: PathBuf) -> Result<()> {
    let folder = match real(&folder) {
        Ok(f) => f,
        Err(_) => std::path::absolute(&folder).with_context(|| format!("{}", folder.display()))?,
    };
    let caller = Caller::sign_in(cfg, None).await?;
    let id = this_machine(&caller).await?;
    let gone = caller.revoke(&id, &folder.to_string_lossy()).await?;
    println!(
        "{} the web may no longer start runs in {} on this machine ({id})",
        "✓".green(),
        gone.cyan()
    );
    Ok(())
}

/// `hanzo link folders`.
pub async fn list(cfg: &mut Config) -> Result<()> {
    let caller = Caller::sign_in(cfg, None).await?;
    let id = this_machine(&caller).await?;
    let grants = caller.folders(&id).await?;
    if grants.is_empty() {
        println!("no folder on this machine ({id}) is granted to the web: `hanzo link grant <folder>` grants one");
    }
    for g in &grants {
        println!(
            "{}  granted by {} at {}",
            g.folder.cyan(),
            g.granted_by,
            g.granted_at
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::network::tests::{caller, token, Fake};
    use super::*;
    use serde_json::Value;

    #[tokio::test]
    async fn grant_posts_the_folder_to_the_machine() {
        let fake = Fake::serve(
            201,
            r#"{"folder":"/home/me/site","grantedBy":"me","grantedAt":"2026-11-21T00:00:00Z"}"#,
        )
        .await;
        let g = caller(&fake.base, Some("acme"))
            .grant("tgt_1", "/home/me/site")
            .await
            .unwrap();
        assert_eq!(
            (g.folder.as_str(), g.granted_by.as_str()),
            ("/home/me/site", "me")
        );
        let (method, path, auth, org, body) = fake.one();
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("POST", "/v1/agent/targets/tgt_1/folders")
        );
        assert_eq!(auth, format!("Bearer {}", token()));
        assert_eq!(org, "acme");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"folder": "/home/me/site"})
        );
    }

    #[tokio::test]
    async fn revoke_names_the_folder_in_the_query() {
        let fake = Fake::serve(200, r#"{"revoked":true,"folder":"/home/me/my site"}"#).await;
        let gone = caller(&fake.base, None)
            .revoke("tgt_1", "/home/me/my site")
            .await
            .unwrap();
        assert_eq!(gone, "/home/me/my site");
        let (method, path, ..) = fake.one();
        assert_eq!(
            (method.as_str(), path.as_str()),
            (
                "DELETE",
                "/v1/agent/targets/tgt_1/folders?folder=%2Fhome%2Fme%2Fmy+site"
            )
        );
    }

    #[tokio::test]
    async fn folders_lists_the_active_grants() {
        let fake = Fake::serve(200, r#"{"folders":[{"folder":"/a","grantedBy":"me","grantedAt":"t"},{"folder":"/b","grantedBy":"me","grantedAt":"t"}]}"#).await;
        let list = caller(&fake.base, None).folders("tgt_1").await.unwrap();
        assert_eq!(
            list.iter().map(|g| g.folder.as_str()).collect::<Vec<_>>(),
            ["/a", "/b"]
        );
        let (method, path, ..) = fake.one();
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("GET", "/v1/agent/targets/tgt_1/folders")
        );
    }

    /// A colleague's machine, or an admin who is not its owner: cloud's 403 is the answer.
    #[tokio::test]
    async fn a_grant_by_someone_not_the_owner_is_cloud_s_refusal() {
        let fake = Fake::serve(
            403,
            r#"{"error":"only the machine's owner may grant or revoke its folders"}"#,
        )
        .await;
        let err = caller(&fake.base, None)
            .grant("tgt_1", "/x")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("403") && err.contains("only the machine's owner"),
            "{err}"
        );
    }

    #[test]
    fn a_folder_is_sent_as_its_real_path() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir_all(site.join("src")).unwrap();
        assert_eq!(
            real(&site.join("src").join("..")).unwrap(),
            std::fs::canonicalize(&site).unwrap()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&site, tmp.path().join("link")).unwrap();
            assert_eq!(
                real(&tmp.path().join("link")).unwrap(),
                std::fs::canonicalize(&site).unwrap()
            );
        }
        assert!(real(&tmp.path().join("gone")).is_err());
        std::fs::write(site.join("f"), "").unwrap();
        assert!(real(&site.join("f")).is_err(), "a file is not a folder");
        assert!(folders_path("../x").is_err());
    }
}
