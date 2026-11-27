//! Workload manifests, static `hanzo.config.{json,ts}` discovery, and OCI
//! registry policy for `hanzo up`.

use anyhow::{bail, Context, Result};
use std::path::Path;

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

/// The cluster's ground: the namespace the workload lands in.
pub(crate) fn ground() -> String {
    "apiVersion: v1
kind: Namespace
metadata:
  name: hanzo
"
    .to_string()
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
    let cloud_memory_mi = (boot.memory_mb / 2).clamp(1024, 4096);
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
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
      containers:
        - name: cloud
          image: {image}
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
{extra_env}          resources:
            requests:
              memory: {cloud_memory_mi}Mi
          readinessProbe:
            httpGet:
              path: /readyz
              port: {CLOUD_PORT}
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
          emptyDir: {{}}
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
        || boot.staged.is_some()
        || boot.iam_issuer.is_some()
        || boot.ai_base_url.is_some()
}
