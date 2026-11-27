//! Workload manifests, static `hanzo.config.{json,ts}` discovery, and OCI
//! registry policy for `hanzo up`.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::budget;
use super::{Boot, CLOUD, CLOUD_NODE_PORT, CLOUD_OCI, CLOUD_PORT, DATA};
use crate::image;

/// Declarative subset of `hanzo.config.json` / `hanzo.config.ts` read
/// statically without ever executing TypeScript.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppManifest {
    pub name: Option<String>,
    pub image: Option<String>,
    pub port: Option<u16>,
    pub cloud: Option<String>,
    pub staged: Option<String>,
    pub iam_issuer: Option<String>,
    pub ai_base_url: Option<String>,
    #[serde(default)]
    pub strict_registry: bool,
    /// How this directory becomes a Cloud image on this machine (`hanzo up --local`).
    pub local: Option<LocalBuild>,
}

/// A checkout's own build of Cloud: the command that makes it and where it leaves
/// the image. Read only when the person asks for `--local`; a `hanzo.config.ts`
/// cannot declare one, since nothing here runs TypeScript to find it.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalBuild {
    /// Run with `sh -c` in the directory that holds the config. Optional: a layout
    /// something else keeps current needs no command.
    pub build: Option<String>,
    /// The OCI image layout directory the build writes, relative to the config.
    pub image: String,
}

/// Returns true when `reference` targets a Hanzo-owned OCI registry.
pub fn is_hanzo_registry(reference: &str) -> bool {
    let Ok(parsed) = image::parse(reference) else {
        return false;
    };
    matches!(
        parsed.registry.to_ascii_lowercase().as_str(),
        "oci.hanzo.ai" | "pkg.hanzo.ai" | "forge.hanzo.ai" | "git.hanzo.ai"
    )
}

/// Choose the Cloud OCI image reference under Hanzo registry policy:
/// - In strict mode (`--strict-registry`), defaults to `CLOUD_OCI` and refuses
///   any non-Hanzo registry reference with zero silent GHCR fallback.
/// - Otherwise, when the caller left the default `CLOUD` and Hanzo OCI
///   credentials are available, upgrades to `CLOUD_OCI`.
pub fn select_cloud_image(
    requested: &str,
    strict_registry: bool,
    has_hanzo_oci_creds: bool,
) -> Result<String> {
    let chosen = if requested == CLOUD && (strict_registry || has_hanzo_oci_creds) {
        CLOUD_OCI.to_string()
    } else {
        requested.to_string()
    };
    if strict_registry && !is_hanzo_registry(&chosen) {
        bail!(
            "strict Hanzo registry mode refuses non-Hanzo image `{chosen}`; \
             use a digest or tag on `oci.hanzo.ai` (e.g. `{CLOUD_OCI}`)"
        );
    }
    Ok(chosen)
}

/// Statically discover an optional `hanzo.config.json` or `hanzo.config.ts`
/// in `dir`. Never executes JavaScript or TypeScript.
pub fn discover_app_manifest(dir: &Path) -> Result<Option<AppManifest>> {
    let json_path = dir.join("hanzo.config.json");
    if json_path.is_file() {
        let raw = std::fs::read_to_string(&json_path)
            .with_context(|| format!("reading {}", json_path.display()))?;
        let parsed: AppManifest = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", json_path.display()))?;
        return Ok(Some(parsed));
    }
    let ts_path = dir.join("hanzo.config.ts");
    if ts_path.is_file() {
        let raw = std::fs::read_to_string(&ts_path)
            .with_context(|| format!("reading {}", ts_path.display()))?;
        return Ok(Some(parse_static_ts_manifest(&raw)));
    }
    Ok(None)
}

/// Extract simple declarative string/bool/int fields from `hanzo.config.ts`
/// without executing arbitrary code.
pub(crate) fn parse_static_ts_manifest(src: &str) -> AppManifest {
    let extract_raw = |key: &str| -> Option<String> {
        for line in src.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            let Some(rest) = trimmed
                .strip_prefix(key)
                .or_else(|| trimmed.strip_prefix(&format!("\"{key}\"")))
                .or_else(|| trimmed.strip_prefix(&format!("'{key}'")))
            else {
                continue;
            };
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix(':') else {
                continue;
            };
            return Some(rest.trim().trim_end_matches(',').trim().to_string());
        }
        None
    };
    let extract_str = |key: &str| -> Option<String> {
        let val = extract_raw(key)?;
        let inner = val
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
        (!inner.is_empty()).then(|| inner.to_string())
    };
    let port = extract_raw("port").and_then(|v| v.parse::<u16>().ok());
    let strict_registry = src.lines().any(|line| {
        let t = line.trim();
        !t.starts_with("//")
            && (t.starts_with("strictRegistry:") || t.starts_with("\"strictRegistry\":"))
            && t.contains("true")
    });
    AppManifest {
        name: extract_str("name"),
        image: extract_str("image"),
        port,
        cloud: extract_str("cloud"),
        staged: extract_str("staged"),
        iam_issuer: extract_str("iamIssuer"),
        ai_base_url: extract_str("aiBaseUrl"),
        strict_registry,
        local: None,
    }
}

pub(crate) fn yaml_env_block(name: &str, value: &str) -> Result<String> {
    if value.chars().any(|c| c.is_control()) {
        bail!("{name} must not contain control characters");
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!(
        "            - name: {name}\n              value: \"{escaped}\"\n"
    ))
}

/// Images this machine built carry this registry name, which is no registry: the
/// name cannot be pulled, so an image under it is one that was loaded.
pub(crate) const LOCAL_REGISTRY: &str = "hanzo.local";

/// Whether a reference names an image this machine built.
pub(crate) fn is_local(reference: &str) -> bool {
    reference
        .strip_prefix(LOCAL_REGISTRY)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The account Cloud's pod runs as. Its own, so what it may ask the API server is
/// what is granted below and nothing an unrelated pod in the namespace inherits.
pub(crate) const ACCOUNT: &str = "cloud";

/// The volume Cloud's data is kept on.
pub(crate) const DATA_CLAIM: &str = "cloud-data";

/// Where a build line would run. Cloud reads it; nothing here populates it.
const BUILD_NAMESPACE: &str = "hanzo-build";

/// The cluster's ground: everything the workload stands on and none of the
/// workload. The namespace, the account the pod runs as, what that account may ask
/// the API server, and the volume its data outlives a pod on.
///
/// **The grants are the requests Cloud makes, verb for verb.** Each was read off a
/// refusal in the pod's log when it ran as the namespace's default account:
///
/// - `pods` list and watch, cluster-wide: the KMS socket places a caller by the pod
///   it runs in, from a watch of the pods on this node (`NODE_NAME` narrows the
///   watch; RBAC cannot, so the grant is the cluster's).
/// - `jobs` list in its own namespace: the runner queue counts the runners in flight.
/// - `jobs` list in `hanzo-build`: the build line is read before a build is admitted.
///
/// Reading, in every case. A local runtime has no builders and no runners, so the
/// verbs that would start one are not granted, and asking for one is refused by
/// name rather than half done.
pub(crate) fn ground(data_mb: u64) -> String {
    format!(
        "apiVersion: v1
kind: Namespace
metadata:
  name: hanzo
---
apiVersion: v1
kind: Namespace
metadata:
  name: {BUILD_NAMESPACE}
---
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {ACCOUNT}
  namespace: hanzo
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: hanzo-cloud
rules:
  - apiGroups: [\"\"]
    resources: [\"pods\"]
    verbs: [\"list\", \"watch\"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: hanzo-cloud
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: hanzo-cloud
subjects:
  - kind: ServiceAccount
    name: {ACCOUNT}
    namespace: hanzo
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: {ACCOUNT}
  namespace: hanzo
rules:
  - apiGroups: [\"batch\"]
    resources: [\"jobs\"]
    verbs: [\"list\"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: {ACCOUNT}
  namespace: hanzo
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: {ACCOUNT}
subjects:
  - kind: ServiceAccount
    name: {ACCOUNT}
    namespace: hanzo
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: {ACCOUNT}
  namespace: {BUILD_NAMESPACE}
rules:
  - apiGroups: [\"batch\"]
    resources: [\"jobs\"]
    verbs: [\"list\"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: {ACCOUNT}
  namespace: {BUILD_NAMESPACE}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: {ACCOUNT}
subjects:
  - kind: ServiceAccount
    name: {ACCOUNT}
    namespace: hanzo
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {DATA_CLAIM}
  namespace: hanzo
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: {data_mb}Mi
"
    )
}

/// The ground for a runtime of this shape. It is laid under every runtime, one
/// too small for Cloud included: a runtime started for something else still has a
/// namespace, and the size check belongs to the workload.
pub(crate) fn ground_for(boot: &Boot) -> String {
    ground(budget::data_mb(boot.disk_mb))
}

#[cfg(test)]
pub(crate) fn workload(image: &str) -> String {
    workload_for(&Boot {
        cpus: 4,
        memory_mb: 4096,
        disk_mb: 16384,
        cloud: image.to_string(),
        staged: None,
        iam_issuer: None,
        ai_base_url: None,
        strict_registry: false,
        local: None,
    })
    .expect("default workload renders")
}

pub(crate) fn workload_for(boot: &Boot) -> Result<String> {
    let image = &boot.cloud;
    let mut extra_env = String::new();
    extra_env.push_str(&yaml_env_block("IAM_DEV_BOOTSTRAP", "1")?);
    if let Some(v) = boot
        .staged
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra_env.push_str(&yaml_env_block("CLOUD_ENABLE_STAGED", v)?);
    }
    if let Some(v) = boot
        .iam_issuer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra_env.push_str(&yaml_env_block("CLOUD_IAM_ISSUER", v)?);
    } else {
        extra_env.push_str(&yaml_env_block("IAM_DEV_HOST_RELATIVE", "1")?);
    }
    if let Some(v) = boot
        .ai_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra_env.push_str(&yaml_env_block("CLOUD_AI_BASE_URL", v)?);
    }
    // The pod's share of the node, the bus's share of the disk: budget.rs.
    let budget = budget::plan(boot.memory_mb, boot.disk_mb)?;
    let cloud_memory_mi = budget.cloud_mb;
    let store_bytes = budget.store_bytes;
    // An image this machine built is in the node's store or it is nowhere: the
    // kubelet is told never to pull, so a missing one is `ErrImageNeverPull` and
    // not a registry's image under the same name. Decided by the name, which is
    // the fact about the image; how this invocation came by it is not.
    let pull_policy = if is_local(image) { "Never" } else { "IfNotPresent" };
    Ok(format!(
        "apiVersion: apps/v1
kind: Deployment
metadata:
  name: cloud
  namespace: hanzo
spec:
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app: cloud
  template:
    metadata:
      labels:
        app: cloud
    spec:
      serviceAccountName: {ACCOUNT}
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
      containers:
        - name: cloud
          image: {image}
          imagePullPolicy: {pull_policy}
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop:
                - ALL
          ports:
            - name: api
              containerPort: {CLOUD_PORT}
          env:
            - name: ZIP_RUNTIME_DIR
              value: /var/lib/cloud/run
            - name: CLOUD_KMS_MASTER_KEY_REF
              valueFrom:
                secretKeyRef:
                  name: cloud
                  key: master
            - name: CLOUD_MEMORY_REQUEST_MIB
              valueFrom:
                resourceFieldRef:
                  containerName: cloud
                  resource: requests.memory
                  divisor: 1Mi
            - name: NODE_NAME
              valueFrom:
                fieldRef:
                  fieldPath: spec.nodeName
            - name: CLOUD_PUBSUB_MAX_STORE
              value: \"{store_bytes}\"
{extra_env}          resources:
            requests:
              memory: {cloud_memory_mi}Mi
          readinessProbe:
            httpGet:
              path: /readyz
              port: {CLOUD_PORT}
            initialDelaySeconds: 3
            periodSeconds: 5
            failureThreshold: 60
          livenessProbe:
            httpGet:
              path: /healthz
              port: {CLOUD_PORT}
            periodSeconds: 10
            failureThreshold: 30
          volumeMounts:
            - name: data
              mountPath: /var/lib/cloud
            - name: tmp
              mountPath: /tmp
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: {DATA_CLAIM}
        - name: tmp
          emptyDir: {{}}
---
apiVersion: v1
kind: Service
metadata:
  name: cloud
  namespace: hanzo
spec:
  type: NodePort
  selector:
    app: cloud
  ports:
    - name: api
      port: {CLOUD_PORT}
      targetPort: {CLOUD_PORT}
      nodePort: {CLOUD_NODE_PORT}
"
    ))
}

pub(crate) fn k3s_asset() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "k3s-arm64"
    } else {
        "k3s"
    }
}

pub(crate) fn k3s_airgap_asset() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "k3s-airgap-images-arm64.tar.zst"
    } else {
        "k3s-airgap-images-amd64.tar.zst"
    }
}

pub(crate) fn is_hanzo_artifact_url(url: &str) -> bool {
    let trimmed = url.trim();
    [
        "https://oci.hanzo.ai/",
        "https://pkg.hanzo.ai/",
        "https://hanzo.sh/",
        "https://get.hanzo.ai/",
        "https://releases.hanzo.ai/",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

pub(crate) fn install_cmd_for(
    strict_registry: bool,
    k3s_url_override: Option<&str>,
) -> Result<String> {
    if let Some(u) = k3s_url_override.map(str::trim).filter(|s| !s.is_empty()) {
        if strict_registry && !is_hanzo_artifact_url(u) {
            bail!(
                "refusing non-Hanzo k3s artifact URL '{u}' under --strict-registry; \
                 set HANZO_K3S_URL to a verified Hanzo-owned endpoint (pkg.hanzo.ai / oci.hanzo.ai)"
            );
        }
        return Ok(format!(
            "curl -fsSLo /usr/local/bin/k3s '{u}' && chmod +x /usr/local/bin/k3s"
        ));
    }
    if strict_registry {
        bail!(
            "refusing GitHub k3s download under --strict-registry; \
             set HANZO_K3S_URL to a verified Hanzo-owned k3s mirror (pkg.hanzo.ai / oci.hanzo.ai)"
        );
    }
    let bin_url = format!(
        "https://github.com/k3s-io/k3s/releases/latest/download/{}",
        k3s_asset()
    );
    let airgap = k3s_airgap_asset();
    let airgap_url = format!("https://github.com/k3s-io/k3s/releases/latest/download/{airgap}");
    Ok(format!(
        "curl -fsSLo /usr/local/bin/k3s '{bin_url}' && chmod +x /usr/local/bin/k3s && \
         mkdir -p {DATA}/agent/images && \
         curl -fsSLo {DATA}/agent/images/{airgap} '{airgap_url}'"
    ))
}

pub(crate) fn install_cmd(strict_registry: bool) -> Result<String> {
    let env_url = std::env::var("HANZO_K3S_URL").ok();
    install_cmd_for(strict_registry, env_url.as_deref())
}

pub(crate) fn has_workload_overrides(boot: &Boot) -> bool {
    boot.cloud != CLOUD
        || boot.local.is_some()
        || boot.staged.is_some()
        || boot.iam_issuer.is_some()
        || boot.ai_base_url.is_some()
}
