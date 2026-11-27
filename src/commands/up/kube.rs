//! Native Rust Kubernetes API client over `~/.kube/hanzo.yaml` mTLS (or bearer
//! token for native Linux k3s), keeping k3s warm across `hanzo down` / `hanzo up`
//! cycles and driving multi-project namespace/deployment/service reconciliation.

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::time::Duration;

pub(crate) struct KubeAuth {
    pub(crate) server: String,
    pub(crate) ca_pem: Option<Vec<u8>>,
    pub(crate) client_pem: Option<Vec<u8>>,
    pub(crate) bearer_token: Option<String>,
    pub(crate) insecure_skip_tls: bool,
}

pub(crate) struct KubeClient {
    pub(crate) server: String,
    http: reqwest::Client,
    bearer_token: Option<String>,
}

impl KubeClient {
    pub(crate) fn parse_kubeconfig(yaml: &str) -> Result<KubeAuth> {
        let doc: serde_norway::Value =
            serde_norway::from_str(yaml).context("parsing kubeconfig YAML")?;
        let cluster = doc
            .get("clusters")
            .and_then(|v| v.as_sequence())
            .and_then(|s| s.first())
            .and_then(|c| c.get("cluster"))
            .ok_or_else(|| anyhow!("kubeconfig missing clusters[0].cluster"))?;
        let server = cluster
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("kubeconfig missing cluster.server"))?
            .trim_end_matches('/')
            .to_string();
        let insecure_skip_tls = cluster
            .get("insecure-skip-tls-verify")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;

        let ca_pem = match cluster
            .get("certificate-authority-data")
            .and_then(|v| v.as_str())
        {
            Some(ca_b64) => Some(
                b64.decode(ca_b64.trim())
                    .context("decoding kubeconfig CA cert")?,
            ),
            None => match cluster
                .get("certificate-authority")
                .and_then(|v| v.as_str())
            {
                Some(path) => Some(
                    std::fs::read(path)
                        .with_context(|| format!("reading kubeconfig CA file {path}"))?,
                ),
                None if insecure_skip_tls => None,
                None => bail!("kubeconfig missing certificate-authority-data"),
            },
        };

        let user = doc
            .get("users")
            .and_then(|v| v.as_sequence())
            .and_then(|s| s.first())
            .and_then(|u| u.get("user"))
            .ok_or_else(|| anyhow!("kubeconfig missing users[0].user"))?;

        let bearer_token = user
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let cert_b64 = user
            .get("client-certificate-data")
            .and_then(|v| v.as_str());
        let key_b64 = user.get("client-key-data").and_then(|v| v.as_str());

        let client_pem = match (cert_b64, key_b64) {
            (Some(c), Some(k)) => {
                let mut pem = b64
                    .decode(c.trim())
                    .context("decoding kubeconfig client cert")?;
                if !pem.ends_with(b"\n") {
                    pem.push(b'\n');
                }
                let key_pem = b64
                    .decode(k.trim())
                    .context("decoding kubeconfig client key")?;
                pem.extend_from_slice(&key_pem);
                Some(pem)
            }
            _ if bearer_token.is_some() => None,
            (None, _) => bail!("kubeconfig missing client-certificate-data"),
            (_, None) => bail!("kubeconfig missing client-key-data"),
        };

        Ok(KubeAuth {
            server,
            ca_pem,
            client_pem,
            bearer_token,
            insecure_skip_tls,
        })
    }

    pub(crate) fn from_kubeconfig(path: &Path) -> Result<Self> {
        let yaml = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let auth = Self::parse_kubeconfig(&yaml)?;
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(15));
        if auth.insecure_skip_tls {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(ca_bytes) = &auth.ca_pem {
            let ca = reqwest::Certificate::from_pem(ca_bytes)
                .context("loading kubeconfig CA certificate")?;
            builder = builder.add_root_certificate(ca);
        }
        if let Some(client_bytes) = &auth.client_pem {
            let identity = reqwest::Identity::from_pem(client_bytes)
                .context("loading kubeconfig client identity")?;
            builder = builder.identity(identity);
        }
        let http = builder.build().context("building k3s API client")?;
        Ok(Self {
            server: auth.server,
            http,
            bearer_token: auth.bearer_token,
        })
    }

    fn auth_req(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.bearer_token {
            Some(tok) => req.bearer_auth(tok),
            None => req,
        }
    }

    pub(crate) async fn deployment_replicas(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{namespace}/deployments/{name}/scale",
            self.server
        );
        let resp = self
            .auth_req(self.http.get(&url))
            .send()
            .await
            .with_context(|| format!("reading {namespace}/{name} scale"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("k3s GET scale returned {status}: {}", body.trim());
        }
        let v: serde_json::Value = resp.json().await.context("decoding deployment scale")?;
        Ok(v.pointer("/spec/replicas")
            .and_then(|r| r.as_u64())
            .map(|r| r as u32))
    }

    pub(crate) async fn cloud_replicas(&self) -> Result<Option<u32>> {
        self.deployment_replicas("hanzo", "cloud").await
    }

    pub(crate) async fn scale_deployment(
        &self,
        namespace: &str,
        name: &str,
        replicas: u32,
    ) -> Result<()> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{namespace}/deployments/{name}/scale",
            self.server
        );
        let resp = self
            .auth_req(self.http.patch(&url))
            .header("Content-Type", "application/merge-patch+json")
            .json(&serde_json::json!({ "spec": { "replicas": replicas } }))
            .send()
            .await
            .with_context(|| format!("scaling {namespace}/{name} deployment"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("k3s PATCH scale returned {status}: {}", body.trim());
        }
        Ok(())
    }

    pub(crate) async fn scale_cloud(&self, replicas: u32) -> Result<()> {
        self.scale_deployment("hanzo", "cloud", replicas).await
    }

    pub(crate) async fn ensure_namespace(&self, namespace: &str) -> Result<()> {
        let get_url = format!("{}/api/v1/namespaces/{namespace}", self.server);
        let resp = self
            .auth_req(self.http.get(&get_url))
            .send()
            .await
            .with_context(|| format!("checking namespace {namespace}"))?;
        if resp.status().is_success() {
            return Ok(());
        }
        let create_url = format!("{}/api/v1/namespaces", self.server);
        let create = self
            .auth_req(self.http.post(&create_url))
            .json(&serde_json::json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": namespace }
            }))
            .send()
            .await
            .with_context(|| format!("creating namespace {namespace}"))?;
        if !create.status().is_success()
            && create.status() != reqwest::StatusCode::CONFLICT
        {
            let status = create.status();
            let body = create.text().await.unwrap_or_default();
            bail!("k3s POST namespace {namespace} returned {status}: {}", body.trim());
        }
        Ok(())
    }

    pub(crate) async fn apply_deployment_doc(
        &self,
        namespace: &str,
        name: &str,
        deploy_yaml: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{namespace}/deployments/{name}?fieldManager=hanzo-cli&force=true",
            self.server
        );
        let resp = self
            .auth_req(self.http.patch(&url))
            .header("Content-Type", "application/apply-patch+yaml")
            .body(deploy_yaml.to_string())
            .send()
            .await
            .with_context(|| format!("applying deployment {namespace}/{name}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let create_url =
                format!("{}/apis/apps/v1/namespaces/{namespace}/deployments", self.server);
            let doc: serde_json::Value = serde_norway::from_str(deploy_yaml)
                .context("converting deployment YAML to JSON")?;
            let create = self
                .auth_req(self.http.post(&create_url))
                .json(&doc)
                .send()
                .await
                .with_context(|| format!("creating deployment {namespace}/{name}"))?;
            if !create.status().is_success() {
                let status = create.status();
                let body = create.text().await.unwrap_or_default();
                bail!("k3s POST deployment returned {status}: {}", body.trim());
            }
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("k3s PATCH deployment returned {status}: {}", body.trim());
        }
        Ok(())
    }

    pub(crate) async fn apply_service_doc(
        &self,
        namespace: &str,
        name: &str,
        svc_yaml: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v1/namespaces/{namespace}/services/{name}?fieldManager=hanzo-cli&force=true",
            self.server
        );
        let resp = self
            .auth_req(self.http.patch(&url))
            .header("Content-Type", "application/apply-patch+yaml")
            .body(svc_yaml.to_string())
            .send()
            .await
            .with_context(|| format!("applying service {namespace}/{name}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let create_url = format!("{}/api/v1/namespaces/{namespace}/services", self.server);
            let doc: serde_json::Value =
                serde_norway::from_str(svc_yaml).context("converting service YAML to JSON")?;
            let create = self
                .auth_req(self.http.post(&create_url))
                .json(&doc)
                .send()
                .await
                .with_context(|| format!("creating service {namespace}/{name}"))?;
            if !create.status().is_success() {
                let status = create.status();
                let body = create.text().await.unwrap_or_default();
                bail!("k3s POST service returned {status}: {}", body.trim());
            }
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("k3s PATCH service returned {status}: {}", body.trim());
        }
        Ok(())
    }

    pub(crate) async fn apply_workload(&self, workload_yaml: &str) -> Result<()> {
        self.ensure_namespace("hanzo").await?;
        let deploy_yaml = extract_deployment_yaml(workload_yaml)?;
        self.apply_deployment_doc("hanzo", "cloud", deploy_yaml).await?;
        if let Ok(svc_yaml) = extract_service_yaml(workload_yaml) {
            self.apply_service_doc("hanzo", "cloud", svc_yaml).await?;
        }
        Ok(())
    }

    /// Apply a multi-document YAML bundle (`Namespace`, `Deployment`, `Service`)
    /// for an isolated application namespace (`hanzo-app-<slug>`) or platform service.
    pub(crate) async fn apply_manifest_bundle(&self, namespace: &str, yaml: &str) -> Result<()> {
        self.ensure_namespace(namespace).await?;
        for raw_doc in yaml.split("\n---\n") {
            let trimmed = raw_doc.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value =
                serde_norway::from_str(trimmed).context("parsing manifest document")?;
            let kind = parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let name = parsed
                .pointer("/metadata/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let doc_ns = parsed
                .pointer("/metadata/namespace")
                .and_then(|v| v.as_str())
                .unwrap_or(namespace);
            match kind {
                "Namespace" if !name.is_empty() => {
                    self.ensure_namespace(name).await?;
                }
                "Deployment" if !name.is_empty() => {
                    self.apply_deployment_doc(doc_ns, name, trimmed).await?;
                }
                "Service" if !name.is_empty() => {
                    self.apply_service_doc(doc_ns, name, trimmed).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Read recent pod logs for `label_selector` (e.g. `app=solar-crm`) in `namespace`.
    pub(crate) async fn pod_logs(
        &self,
        namespace: &str,
        label_selector: &str,
        tail_lines: u32,
    ) -> Result<String> {
        let pods_url = format!(
            "{}/api/v1/namespaces/{namespace}/pods?labelSelector={label_selector}",
            self.server
        );
        let resp = self
            .auth_req(self.http.get(&pods_url))
            .send()
            .await
            .with_context(|| format!("listing pods in {namespace}"))?;
        if !resp.status().is_success() {
            bail!("k3s GET pods returned {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await.context("decoding pod list")?;
        let pod_name = v
            .pointer("/items/0/metadata/name")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("no pods found in namespace `{namespace}` matching `{label_selector}`"))?;
        let log_url = format!(
            "{}/api/v1/namespaces/{namespace}/pods/{pod_name}/log?tailLines={tail_lines}",
            self.server
        );
        let log_resp = self
            .auth_req(self.http.get(&log_url))
            .send()
            .await
            .with_context(|| format!("reading pod logs for {namespace}/{pod_name}"))?;
        if !log_resp.status().is_success() {
            bail!("k3s GET pod log returned {}", log_resp.status());
        }
        Ok(log_resp.text().await.unwrap_or_default())
    }
}

pub(crate) fn extract_deployment_yaml(workload_yaml: &str) -> Result<&str> {
    workload_yaml
        .split("\n---\n")
        .find(|doc| doc.contains("kind: Deployment"))
        .map(str::trim)
        .filter(|doc| !doc.is_empty())
        .ok_or_else(|| anyhow!("workload manifest missing Deployment document"))
}

pub(crate) fn extract_service_yaml(workload_yaml: &str) -> Result<&str> {
    workload_yaml
        .split("\n---\n")
        .find(|doc| doc.contains("kind: Service"))
        .map(str::trim)
        .filter(|doc| !doc.is_empty())
        .ok_or_else(|| anyhow!("workload manifest missing Service document"))
}
