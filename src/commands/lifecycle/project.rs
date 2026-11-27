//! Persistent desired-state store (`~/.hanzo/lifecycle/state.json`) and
//! project-aware multi-application detection & Kubernetes manifest generation.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::commands::up;

pub const DEFAULT_CPUS: u32 = 4;
pub const DEFAULT_MEMORY_MB: u64 = 4096;
pub const DEFAULT_DISK_MB: u64 = 32768;

/// Persistent desired state across `hanzo start`, `hanzo up`, `hanzo down`,
/// `hanzo stop`, and `hanzo restart`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredState {
    #[serde(default)]
    pub runtime: RuntimeDesired,
    #[serde(default)]
    pub platform: PlatformDesired,
    #[serde(default)]
    pub apps: BTreeMap<String, RegisteredApp>,
    #[serde(default)]
    pub sandboxes: BTreeMap<String, RegisteredSandbox>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for DesiredState {
    fn default() -> Self {
        Self {
            runtime: RuntimeDesired::default(),
            platform: PlatformDesired::default(),
            apps: BTreeMap::new(),
            sandboxes: BTreeMap::new(),
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeDesired {
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
    pub backend: String,
    pub desired_running: bool,
    pub strict_registry: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for RuntimeDesired {
    fn default() -> Self {
        Self {
            cpus: DEFAULT_CPUS,
            memory_mb: DEFAULT_MEMORY_MB,
            disk_mb: DEFAULT_DISK_MB,
            backend: "auto".to_string(),
            desired_running: false,
            strict_registry: false,
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformDesired {
    pub cloud_enabled: bool,
    pub cloud_image: String,
    pub staged: Option<String>,
    pub iam_issuer: Option<String>,
    pub ai_base_url: Option<String>,
    pub engine_enabled: bool,
    pub services: BTreeMap<String, ValueOrBool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Accepts either `true`/`false` (CLI) or `{"desired": "running"|"stopped", ...}` (`hanzod` mirror).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ValueOrBool {
    Bool(bool),
    Obj(serde_json::Value),
}

impl ValueOrBool {
    pub fn is_enabled(&self) -> bool {
        match self {
            ValueOrBool::Bool(b) => *b,
            ValueOrBool::Obj(v) => v
                .get("desired")
                .or_else(|| v.get("actual"))
                .and_then(|s| s.as_str())
                .is_some_and(|s| matches!(s, "running" | "up" | "ready" | "enabled")),
        }
    }
}

impl Default for PlatformDesired {
    fn default() -> Self {
        Self {
            cloud_enabled: false,
            cloud_image: up::CLOUD.to_string(),
            staged: None,
            iam_issuer: None,
            ai_base_url: None,
            engine_enabled: false,
            services: BTreeMap::new(),
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredApp {
    pub slug: String,
    pub name: String,
    pub project_dir: String,
    pub namespace: String,
    pub kind: String,
    pub image: Option<String>,
    pub command: Option<String>,
    pub container_port: u16,
    pub node_port: u16,
    pub desired_running: bool,
    #[serde(default)]
    pub local_pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredSandbox {
    pub name: String,
    pub image: String,
    pub node: String,
    pub desired_running: bool,
    #[serde(default)]
    pub pid: Option<u32>,
}

/// Locate `~/.hanzo/lifecycle` (honoring `HANZO_LIFECYCLE_DIR` for hermetic tests).
pub fn lifecycle_dir() -> Result<PathBuf> {
    let dir = match std::env::var("HANZO_LIFECYCLE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => dirs::home_dir()
            .ok_or_else(|| anyhow!("no home directory"))?
            .join(".hanzo")
            .join("lifecycle"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn state_path() -> Result<PathBuf> {
    Ok(lifecycle_dir()?.join("state.json"))
}

pub fn load_state() -> Result<DesiredState> {
    load_state_from(&state_path()?)
}

pub fn load_state_from(path: &Path) -> Result<DesiredState> {
    if !path.is_file() {
        return Ok(DesiredState::default());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(DesiredState::default());
    }
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save_state(state: &DesiredState) -> Result<()> {
    save_state_to(&state_path()?, state)
}

pub fn save_state_to(path: &Path, state: &DesiredState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(state).context("encoding lifecycle state")?;
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {}", path.display()))?;
    Ok(())
}

/// Detected application metadata from a project directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedProject {
    pub slug: String,
    pub name: String,
    pub project_dir: PathBuf,
    pub kind: String,
    pub image: Option<String>,
    pub command: Option<String>,
    pub container_port: u16,
}

/// Normalize a directory or project name into a valid K8s DNS-1123 label (`solar-crm`).
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "app".to_string()
    } else if trimmed.len() > 40 {
        trimmed[..40].trim_end_matches('-').to_string()
    } else {
        trimmed
    }
}

/// Humanize a slug like `"solar-crm"` into `"Solar CRM"` when no explicit display
/// title was declared.
pub fn humanize_slug(slug: &str) -> String {
    const ACRONYMS: &[&str] = &["crm", "erp", "api", "ui", "ai", "kms", "iam", "sql", "mcp"];
    slug.split('-')
        .filter(|s| !s.is_empty())
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            if ACRONYMS.contains(&lower.as_str()) {
                lower.to_ascii_uppercase()
            } else {
                let mut chars = lower.chars();
                match chars.next() {
                    Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns true if `dir` is a core Hanzo infrastructure checkout (`hanzoai/cloud`,
/// `hanzoai/cli`, `hanzoai/node`, `hanzoai/vm`, `hanzoai/engine`) rather than a
/// user application workload.
pub fn is_system_checkout(dir: &Path) -> bool {
    if let Ok(gomod) = std::fs::read_to_string(dir.join("go.mod")) {
        if let Some(first) = gomod.lines().find(|l| l.starts_with("module ")) {
            let m = first.trim_start_matches("module ").trim();
            if m == "github.com/hanzoai/cloud" || m == "hanzo.ai/cloud" {
                return true;
            }
        }
    }
    if let Ok(cargo) = std::fs::read_to_string(dir.join("Cargo.toml")) {
        for sys_pkg in [
            "name = \"hanzo-cli\"",
            "name = \"hanzod\"",
            "name = \"hanzo-vm\"",
            "name = \"hanzo-gen\"",
        ] {
            if cargo.contains(sys_pkg) {
                return true;
            }
        }
    }
    false
}

/// Statically detect a user application in `dir`.
///
/// Inspects `hanzo.toml`, `hanzo.yaml`/`hanzo.yml`, `hanzo.config.{json,ts}`,
/// `compose.yaml`/`docker-compose.yml`, `Dockerfile`, `package.json`, `Cargo.toml`,
/// and `go.mod`. Returns `Ok(None)` if `dir` has no application manifest or is a
/// Hanzo system checkout (`hanzoai/cloud`, `hanzoai/cli`, etc.).
pub fn detect_project(dir: &Path) -> Result<Option<DetectedProject>> {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !canonical.is_dir() || is_system_checkout(&canonical) {
        return Ok(None);
    }

    let fallback_slug = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .map(slugify)
        .unwrap_or_else(|| "app".into());

    // 1. hanzo.toml
    let toml_path = canonical.join("hanzo.toml");
    if toml_path.is_file() {
        let raw = std::fs::read_to_string(&toml_path)
            .with_context(|| format!("reading {}", toml_path.display()))?;
        let val: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parsing {}", toml_path.display()))?;
        let app_tbl = val.get("app").unwrap_or(&val);
        let raw_name = app_tbl
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let slug = app_tbl
            .get("slug")
            .and_then(|v| v.as_str())
            .map(slugify)
            .or_else(|| raw_name.as_deref().map(slugify))
            .unwrap_or_else(|| fallback_slug.clone());
        let name = raw_name
            .filter(|n| n.contains(' ') || n.chars().any(|c| c.is_ascii_uppercase()))
            .unwrap_or_else(|| humanize_slug(&slug));
        let image = app_tbl
            .get("image")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let command = app_tbl
            .get("command")
            .or_else(|| app_tbl.get("dev"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let port = app_tbl
            .get("port")
            .and_then(|v| v.as_integer())
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(3000);
        return Ok(Some(DetectedProject {
            slug,
            name,
            project_dir: canonical,
            kind: "hanzo.toml".into(),
            image,
            command,
            container_port: port,
        }));
    }

    // 2. hanzo.yaml / hanzo.yml
    for yname in ["hanzo.yaml", "hanzo.yml"] {
        let ypath = canonical.join(yname);
        if ypath.is_file() {
            let raw = std::fs::read_to_string(&ypath)
                .with_context(|| format!("reading {}", ypath.display()))?;
            let val: serde_norway::Value = serde_norway::from_str(&raw)
                .with_context(|| format!("parsing {}", ypath.display()))?;
            let app_tbl = val.get("app").unwrap_or(&val);
            let raw_name = app_tbl
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let slug = raw_name
                .as_deref()
                .map(slugify)
                .unwrap_or_else(|| fallback_slug.clone());
            let name = raw_name
                .filter(|n| n.contains(' ') || n.chars().any(|c| c.is_ascii_uppercase()))
                .unwrap_or_else(|| humanize_slug(&slug));
            let image = app_tbl
                .get("image")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let command = app_tbl
                .get("command")
                .or_else(|| val.get("dev").and_then(|d| d.get("command")))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let port = app_tbl
                .get("port")
                .and_then(|v| v.as_u64())
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(3000);
            return Ok(Some(DetectedProject {
                slug,
                name,
                project_dir: canonical,
                kind: yname.into(),
                image,
                command,
                container_port: port,
            }));
        }
    }

    // 3. hanzo.config.json / hanzo.config.ts
    if let Some(m) = up::discover_app_manifest(&canonical)? {
        if m.name.is_some() || m.image.is_some() || m.port.is_some() {
            let slug = m
                .name
                .as_deref()
                .map(slugify)
                .unwrap_or_else(|| fallback_slug.clone());
            let name = m
                .name
                .filter(|n| n.contains(' ') || n.chars().any(|c| c.is_ascii_uppercase()))
                .unwrap_or_else(|| humanize_slug(&slug));
            return Ok(Some(DetectedProject {
                slug,
                name,
                project_dir: canonical,
                kind: "hanzo.config".into(),
                image: m.image,
                command: None,
                container_port: m.port.unwrap_or(3000),
            }));
        }
    }

    // 4. compose.yaml / docker-compose.yml
    for cname in [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ] {
        let cpath = canonical.join(cname);
        if cpath.is_file() {
            let raw = std::fs::read_to_string(&cpath).unwrap_or_default();
            let (image, port) = parse_compose_primary_service(&raw);
            return Ok(Some(DetectedProject {
                slug: fallback_slug.clone(),
                name: humanize_slug(&fallback_slug),
                project_dir: canonical,
                kind: cname.into(),
                image,
                command: None,
                container_port: port.unwrap_or(8080),
            }));
        }
    }

    // 5. Dockerfile / Containerfile
    for dname in ["Dockerfile", "Containerfile"] {
        let dpath = canonical.join(dname);
        if dpath.is_file() {
            let raw = std::fs::read_to_string(&dpath).unwrap_or_default();
            let port = parse_dockerfile_expose(&raw).unwrap_or(8080);
            return Ok(Some(DetectedProject {
                slug: fallback_slug.clone(),
                name: humanize_slug(&fallback_slug),
                project_dir: canonical,
                kind: dname.into(),
                image: None,
                command: None,
                container_port: port,
            }));
        }
    }

    // 6. package.json
    let pkg_path = canonical.join("package.json");
    if pkg_path.is_file() {
        if let Ok(raw) = std::fs::read_to_string(&pkg_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                let raw_pkg_name = val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.rsplit('/').next().unwrap_or(s));
                let slug = raw_pkg_name
                    .map(slugify)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| fallback_slug.clone());
                let has_dev_or_start = val
                    .pointer("/scripts/dev")
                    .or_else(|| val.pointer("/scripts/start"))
                    .is_some();
                let cmd = if val.pointer("/scripts/dev").is_some() {
                    Some("npm run dev".to_string())
                } else if val.pointer("/scripts/start").is_some() {
                    Some("npm start".to_string())
                } else {
                    None
                };
                if has_dev_or_start || canonical.join("hanzo.config.json").is_file() {
                    return Ok(Some(DetectedProject {
                        name: humanize_slug(&slug),
                        slug,
                        project_dir: canonical,
                        kind: "node".into(),
                        image: None,
                        command: cmd,
                        container_port: 3000,
                    }));
                }
            }
        }
    }

    // 7. Cargo.toml / go.mod / pyproject.toml
    if canonical.join("Cargo.toml").is_file() {
        return Ok(Some(DetectedProject {
            name: humanize_slug(&fallback_slug),
            slug: fallback_slug,
            project_dir: canonical,
            kind: "rust".into(),
            image: None,
            command: Some("cargo run".into()),
            container_port: 8080,
        }));
    }
    if canonical.join("go.mod").is_file() {
        return Ok(Some(DetectedProject {
            name: humanize_slug(&fallback_slug),
            slug: fallback_slug,
            project_dir: canonical,
            kind: "go".into(),
            image: None,
            command: Some("go run .".into()),
            container_port: 8080,
        }));
    }
    if canonical.join("pyproject.toml").is_file() {
        return Ok(Some(DetectedProject {
            name: humanize_slug(&fallback_slug),
            slug: fallback_slug,
            project_dir: canonical,
            kind: "python".into(),
            image: None,
            command: None,
            container_port: 8000,
        }));
    }

    Ok(None)
}

fn parse_compose_primary_service(raw: &str) -> (Option<String>, Option<u16>) {
    let Ok(val) = serde_norway::from_str::<serde_norway::Value>(raw) else {
        return (None, None);
    };
    let Some(services) = val.get("services").and_then(|v| v.as_mapping()) else {
        return (None, None);
    };
    let Some((_, first_svc)) = services.iter().next() else {
        return (None, None);
    };
    let image = first_svc
        .get("image")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let port = first_svc
        .get("ports")
        .and_then(|v| v.as_sequence())
        .and_then(|seq| seq.first())
        .and_then(|p| p.as_str().map(str::to_string).or_else(|| p.as_u64().map(|n| n.to_string())))
        .and_then(|s| {
            let clean = s.split('/').next().unwrap_or(&s);
            clean.rsplit(':').next()?.parse::<u16>().ok()
        });
    (image, port)
}

fn parse_dockerfile_expose(raw: &str) -> Option<u16> {
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("EXPOSE ").or_else(|| t.strip_prefix("expose ")) {
            if let Some(tok) = rest.split_whitespace().next() {
                let num = tok.split('/').next().unwrap_or(tok);
                if let Ok(port) = num.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Allocate a deterministic, non-colliding NodePort (`30100..=32699`) for `slug`.
pub fn allocate_node_port(state: &DesiredState, slug: &str) -> u16 {
    if let Some(existing) = state.apps.get(slug) {
        if (30100..=32699).contains(&existing.node_port) {
            return existing.node_port;
        }
    }
    let used: std::collections::HashSet<u16> =
        state.apps.values().map(|a| a.node_port).collect();
    let mut hash: u32 = 2166136261;
    for b in slug.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    for offset in 0..2600u16 {
        let candidate = 30100 + (((hash as u16).wrapping_add(offset)) % 2600);
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    30100
}

/// Register or update `detected` in `state` with `desired_running = true`.
pub fn upsert_detected_app(state: &mut DesiredState, detected: &DetectedProject) -> RegisteredApp {
    let node_port = allocate_node_port(state, &detected.slug);
    let existing_pid = state
        .apps
        .get(&detected.slug)
        .and_then(|a| a.local_pid);
    let app = RegisteredApp {
        slug: detected.slug.clone(),
        name: detected.name.clone(),
        project_dir: detected.project_dir.to_string_lossy().into_owned(),
        namespace: format!("hanzo-app-{}", detected.slug),
        kind: detected.kind.clone(),
        image: detected.image.clone(),
        command: detected.command.clone(),
        container_port: detected.container_port,
        node_port,
        desired_running: true,
        local_pid: existing_pid,
    };
    state.apps.insert(detected.slug.clone(), app.clone());
    app
}

/// Resolve an application by optional `query` (`solar` matches `solar-crm` / `Solar CRM`)
/// or by `cwd` when `query` is `None`.
pub fn resolve_registered_app<'a>(
    state: &'a DesiredState,
    query: Option<&str>,
    cwd: Option<&Path>,
) -> Result<&'a RegisteredApp> {
    if let Some(q) = query.map(str::trim).filter(|s| !s.is_empty()) {
        let q_slug = slugify(q);
        if let Some(exact) = state.apps.get(&q_slug) {
            return Ok(exact);
        }
        let q_lower = q.to_ascii_lowercase();
        let matches: Vec<&RegisteredApp> = state
            .apps
            .values()
            .filter(|a| {
                a.slug.starts_with(&q_slug)
                    || a.name.to_ascii_lowercase() == q_lower
                    || a.name.to_ascii_lowercase().starts_with(&q_lower)
            })
            .collect();
        return match matches.as_slice() {
            [single] => Ok(*single),
            [] => bail!(
                "no registered application matching `{q}` (run `hanzo app list` or `hanzo up` inside the project directory)"
            ),
            many => {
                let names: Vec<&str> = many.iter().map(|a| a.slug.as_str()).collect();
                bail!(
                    "application `{q}` is ambiguous (matches: {}) — specify the full slug",
                    names.join(", ")
                )
            }
        };
    }

    if let Some(dir) = cwd {
        let canonical = std::fs::canonicalize(dir)
            .unwrap_or_else(|_| dir.to_path_buf())
            .to_string_lossy()
            .into_owned();
        if let Some(found) = state.apps.values().find(|a| a.project_dir == canonical) {
            return Ok(found);
        }
    }

    bail!("no application specified and current directory is not a registered Hanzo app — pass `<name>` or run `hanzo app list`")
}

/// Render an isolated Kubernetes Namespace + Deployment + NodePort Service manifest
/// for a containerized user application (`hanzo-app-<slug>`).
pub fn render_app_k8s_manifest(app: &RegisteredApp, image: &str) -> String {
    let ns = &app.namespace;
    let slug = &app.slug;
    let port = app.container_port;
    let node_port = app.node_port;
    format!(
        "apiVersion: v1
kind: Namespace
metadata:
  name: {ns}
  labels:
    hanzo.ai/managed-by: hanzo-cli
    hanzo.ai/app: {slug}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {slug}
  namespace: {ns}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {slug}
  template:
    metadata:
      labels:
        app: {slug}
    spec:
      containers:
        - name: {slug}
          image: {image}
          ports:
            - name: http
              containerPort: {port}
          env:
            - name: PORT
              value: \"{port}\"
            - name: HANZO_API_URL
              value: \"http://cloud.hanzo.svc.cluster.local:8080\"
---
apiVersion: v1
kind: Service
metadata:
  name: {slug}
  namespace: {ns}
spec:
  type: NodePort
  selector:
    app: {slug}
  ports:
    - name: http
      port: {port}
      targetPort: {port}
      nodePort: {node_port}
"
    )
}
