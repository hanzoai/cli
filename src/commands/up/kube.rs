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
        Ok(Some(scale_replicas(&v)))
    }

    /// The image the Cloud Deployment names, or `None` when there is no Deployment.
    pub(crate) async fn cloud_image(&self) -> Result<Option<String>> {
        Ok(self
            .get("/apis/apps/v1/namespaces/hanzo/deployments/cloud")
            .await?
            .and_then(|d| {
                d.pointer("/spec/template/spec/containers/0/image")
                    .and_then(|i| i.as_str())
                    .map(str::to_string)
            }))
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

    /// Put Cloud on the cluster: the ground it stands on, then the workload.
    ///
    /// The Deployment is REPLACED, not merged. A merge keeps every field some
    /// earlier writer set and this one no longer states — an `emptyDir` beside the
    /// volume claim that replaced it, which the API server refuses — so what runs
    /// would depend on what ran before. Replaced, it is the manifest.
    pub(crate) async fn apply_workload(&self, ground_yaml: &str, workload_yaml: &str) -> Result<()> {
        self.apply_docs(ground_yaml).await?;
        let deploy_yaml = extract_deployment_yaml(workload_yaml)?;
        self.replace_deployment("hanzo", "cloud", deploy_yaml).await?;
        if let Ok(svc_yaml) = extract_service_yaml(workload_yaml) {
            self.apply_service_doc("hanzo", "cloud", svc_yaml).await?;
        }
        Ok(())
    }

    /// Apply each document of a manifest by its own kind. A volume claim is made
    /// once and then left: its size is not something a later run may restate.
    pub(crate) async fn apply_docs(&self, yaml: &str) -> Result<()> {
        for raw in yaml.split("\n---\n") {
            let doc = raw.trim();
            if doc.is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_norway::from_str(doc).context("parsing a manifest document")?;
            let s = |ptr: &str| v.pointer(ptr).and_then(|x| x.as_str()).unwrap_or("");
            let (api, kind, name, ns) = (
                s("/apiVersion"),
                s("/kind"),
                s("/metadata/name"),
                s("/metadata/namespace"),
            );
            if kind == "Namespace" {
                self.ensure_namespace(name).await?;
                continue;
            }
            let url = resource_url(&self.server, api, kind, ns, name)?;
            if kind == "PersistentVolumeClaim" {
                let have = self
                    .auth_req(self.http.get(&url))
                    .send()
                    .await
                    .with_context(|| format!("reading {kind} {ns}/{name}"))?;
                if have.status().is_success() {
                    continue;
                }
            }
            let resp = self
                .auth_req(self.http.patch(format!("{url}?fieldManager=hanzo-cli&force=true")))
                .header("Content-Type", "application/apply-patch+yaml")
                .body(doc.to_string())
                .send()
                .await
                .with_context(|| format!("applying {kind} {ns}/{name}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!("applying {kind} {name}: {status}: {}", body.trim());
            }
        }
        Ok(())
    }

    /// Make the Deployment exactly `deploy_yaml`: created when absent, replaced
    /// when present. A replace that races another writer is tried once more.
    pub(crate) async fn replace_deployment(
        &self,
        namespace: &str,
        name: &str,
        deploy_yaml: &str,
    ) -> Result<()> {
        let mut doc: serde_json::Value =
            serde_norway::from_str(deploy_yaml).context("converting deployment YAML to JSON")?;
        let all = format!("{}/apis/apps/v1/namespaces/{namespace}/deployments", self.server);
        let one = format!("{all}/{name}");
        for attempt in 0..2 {
            let have = self
                .auth_req(self.http.get(&one))
                .send()
                .await
                .with_context(|| format!("reading deployment {namespace}/{name}"))?;
            let resp = if have.status() == reqwest::StatusCode::NOT_FOUND {
                self.auth_req(self.http.post(&all)).json(&doc).send().await
            } else if have.status().is_success() {
                let live: serde_json::Value =
                    have.json().await.context("decoding the live deployment")?;
                let version = live
                    .pointer("/metadata/resourceVersion")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                doc["metadata"]["resourceVersion"] = version;
                self.auth_req(self.http.put(&one)).json(&doc).send().await
            } else {
                bail!("k3s GET deployment returned {}", have.status());
            }
            .with_context(|| format!("writing deployment {namespace}/{name}"))?;
            if resp.status().is_success() {
                return Ok(());
            }
            let status = resp.status();
            if status == reqwest::StatusCode::CONFLICT && attempt == 0 {
                continue;
            }
            let body = resp.text().await.unwrap_or_default();
            bail!("writing deployment {namespace}/{name}: {status}: {}", body.trim());
        }
        Ok(())
    }

    /// One object, or `None` when the cluster has none by that name.
    pub(crate) async fn get(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let resp = self
            .auth_req(self.http.get(format!("{}{path}", self.server)))
            .send()
            .await
            .with_context(|| format!("reading {path}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("k3s GET {path} returned {}", resp.status());
        }
        Ok(Some(resp.json().await.with_context(|| format!("decoding {path}"))?))
    }

    /// Wait until the Deployment runs what it was last told to: its newest
    /// generation observed, every replica updated and available. A wait that runs
    /// out says what the pod is doing instead, because "not ready" is not a reason.
    pub(crate) async fn rollout(&self, namespace: &str, name: &str, patience: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + patience;
        let path = format!("/apis/apps/v1/namespaces/{namespace}/deployments/{name}");
        loop {
            if let Some(d) = self.get(&path).await? {
                let n = |ptr: &str| d.pointer(ptr).and_then(|x| x.as_i64()).unwrap_or(0);
                let want = n("/spec/replicas");
                if n("/status/observedGeneration") >= n("/metadata/generation")
                    && n("/status/updatedReplicas") == want
                    && n("/status/availableReplicas") == want
                    && n("/status/replicas") == want
                {
                    return Ok(());
                }
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "{namespace}/{name} is not ready after {}s — {}",
                    patience.as_secs(),
                    self.pod_trouble(namespace, &format!("app={name}")).await
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Why the pods behind a selector are not running, in the kubelet's own words.
    pub(crate) async fn pod_trouble(&self, namespace: &str, selector: &str) -> String {
        let path = format!("/api/v1/namespaces/{namespace}/pods?labelSelector={selector}");
        let Ok(Some(list)) = self.get(&path).await else {
            return "its pods could not be read".into();
        };
        let mut said = Vec::new();
        for pod in list.pointer("/items").and_then(|i| i.as_array()).into_iter().flatten() {
            let name = pod.pointer("/metadata/name").and_then(|x| x.as_str()).unwrap_or("?");
            let phase = pod.pointer("/status/phase").and_then(|x| x.as_str()).unwrap_or("?");
            let mut why = String::new();
            for c in pod
                .pointer("/status/containerStatuses")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
            {
                for state in ["waiting", "terminated"] {
                    if let Some(st) = c.pointer(&format!("/state/{state}")) {
                        let g = |k: &str| st.get(k).and_then(|x| x.as_str()).unwrap_or("");
                        why = format!(" {} {}", g("reason"), g("message"));
                    }
                }
            }
            if why.is_empty() {
                for c in pod.pointer("/status/conditions").and_then(|x| x.as_array()).into_iter().flatten() {
                    if c.get("status").and_then(|x| x.as_str()) == Some("False") {
                        let g = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
                        why = format!(" {} {}", g("reason"), g("message"));
                        break;
                    }
                }
            }
            said.push(format!("pod {name} is {phase}:{}", why.trim_end()));
        }
        if said.is_empty() {
            "it has no pod".into()
        } else {
            said.join("; ")
        }
    }

    /// Whether `user` may do this, asked of the API server itself
    /// (a SubjectAccessReview), so a grant is checked the way it is enforced.
    pub(crate) async fn allowed(
        &self,
        user: &str,
        verb: &str,
        group: &str,
        resource: &str,
        namespace: &str,
    ) -> Result<bool> {
        let body = serde_json::json!({
            "apiVersion": "authorization.k8s.io/v1",
            "kind": "SubjectAccessReview",
            "spec": {
                "user": user,
                "resourceAttributes": {
                    "verb": verb, "group": group, "resource": resource, "namespace": namespace
                }
            }
        });
        let resp = self
            .auth_req(self.http.post(format!(
                "{}/apis/authorization.k8s.io/v1/subjectaccessreviews",
                self.server
            )))
            .json(&body)
            .send()
            .await
            .context("asking the API server about a grant")?;
        if !resp.status().is_success() {
            bail!("k3s POST subjectaccessreviews returned {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await.context("decoding the review")?;
        Ok(v.pointer("/status/allowed").and_then(|x| x.as_bool()).unwrap_or(false))
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

/// The API path of one object, from what its own manifest says it is. The kinds are
/// the ones this CLI writes; anything else is a manifest it has no business applying.
pub(crate) fn resource_url(
    server: &str,
    api_version: &str,
    kind: &str,
    namespace: &str,
    name: &str,
) -> Result<String> {
    let (plural, namespaced) = match kind {
        "ServiceAccount" => ("serviceaccounts", true),
        "Service" => ("services", true),
        "PersistentVolumeClaim" => ("persistentvolumeclaims", true),
        "Deployment" => ("deployments", true),
        "Role" => ("roles", true),
        "RoleBinding" => ("rolebindings", true),
        "ClusterRole" => ("clusterroles", false),
        "ClusterRoleBinding" => ("clusterrolebindings", false),
        other => bail!("no API path for a {other}"),
    };
    if name.is_empty() {
        bail!("a {kind} with no name");
    }
    let root = if api_version.contains('/') {
        format!("{server}/apis/{api_version}")
    } else {
        format!("{server}/api/{api_version}")
    };
    Ok(match (namespaced, namespace) {
        (true, "") => bail!("{kind} {name} names no namespace"),
        (true, ns) => format!("{root}/namespaces/{ns}/{plural}/{name}"),
        (false, _) => format!("{root}/{plural}/{name}"),
    })
}

/// The replicas a Scale object states. Kubernetes leaves `spec.replicas` OUT of the
/// document when it is zero, so an absent field is a Deployment scaled to nothing
/// and not the absence of a Deployment — that is a 404, and the caller's `None`.
/// Read the other way, `hanzo up` after `hanzo down` found "no Deployment" and
/// deployed its default image over whatever the stopped one named.
pub(crate) fn scale_replicas(scale: &serde_json::Value) -> u32 {
    scale
        .pointer("/spec/replicas")
        .and_then(|r| r.as_u64())
        .unwrap_or(0) as u32
}
