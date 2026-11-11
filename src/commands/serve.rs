//! `hanzo serve` — native local project detector, dev server, bundler, and preview server.
//!
//! Replaces external frontend dev/build toolchains (`vite`) with a single sovereign
//! command that auto-detects the project in the working directory:
//! - Web SPA / React / TypeScript (`index.html` + `src/main.tsx` / `src/index.tsx` / `src/App.tsx`):
//!   transpiles & bundles TypeScript/TSX/JSX/CSS (`esbuild` when installed, or the built-in
//!   TS/TSX/CSS bundler), serves `index.html` + `public/` + SPA history fallback (`/*`),
//!   streams live-reload events over `GET /__hanzo_hmr`, and proxies `/v1/*`, `/mcp`, and
//!   `/api/*` to `HANZO_API`.
//! - Go `hanzo.ai/zip` service (`go.mod` referencing `hanzo.ai/zip`): runs `go run .` with
//!   live reload on `.go` edits and prints `/docs`, `/.well-known/openapi.json`, and `/mcp`.
//! - Static directory (`index.html` with no TS entrypoint): serves static assets with live
//!   reload and `/v1` API proxy.

use anyhow::{anyhow, bail, Context, Result};
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream as TokioStream};
use tokio::sync::{broadcast, RwLock};

use crate::config::Config;

const HMR_ENDPOINT: &str = "/__hanzo_hmr";

const HMR_CLIENT_SNIPPET: &str = r#"<script data-hanzo-hmr>
(() => {
  if (typeof EventSource === "undefined") return;
  const es = new EventSource("/__hanzo_hmr");
  es.onmessage = (ev) => {
    if (ev.data === "reload") window.location.reload();
  };
})();
</script>"#;

#[derive(clap::Args, Debug, Clone)]
#[command(subcommand_precedence_over_arg = true)]
pub struct Args {
    /// Project directory to serve (defaults to current working directory)
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Local port to listen on
    #[arg(short, long, default_value_t = 3300)]
    pub port: u16,

    /// Local bind address
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Upstream Hanzo API origin for `/v1/*`, `/mcp`, and `/api/*` proxy
    #[arg(long, value_name = "URL")]
    pub api: Option<String>,

    #[command(subcommand)]
    pub command: Option<ServeCommand>,
}

#[derive(clap::Subcommand, Debug, Clone)]
pub enum ServeCommand {
    /// Bundle a local project into an optimized production directory (`dist/`)
    Build {
        /// Project directory to build (defaults to current working directory)
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Output directory (relative to project root or absolute)
        #[arg(short, long, default_value = "dist")]
        out: PathBuf,
    },
    /// Serve a production build (`dist/`) locally with SPA fallback and `/v1` API proxy
    Preview {
        /// Project directory (defaults to current working directory)
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Built output directory to preview
        #[arg(short, long, default_value = "dist")]
        out: PathBuf,

        /// Local port to listen on
        #[arg(short, long, default_value_t = 3300)]
        port: u16,

        /// Local bind address
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Upstream Hanzo API origin for `/v1/*`, `/mcp`, and `/api/*` proxy
        #[arg(long, value_name = "URL")]
        api: Option<String>,
    },
}

/// Detected project archetype in a local directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    /// Web SPA / React / TypeScript (`index.html` + TS/TSX/JSX module entrypoint)
    WebSpa { entry: PathBuf },
    /// Go `hanzo.ai/zip` application (`go.mod` referencing `hanzo.ai/zip`)
    GoZip,
    /// Static directory (`index.html` without a TypeScript/JSX entrypoint)
    Static,
}

/// Result of bundling a Web SPA or static project into an output directory.
#[derive(Debug, Clone)]
pub struct BuildReport {
    pub kind: ProjectKind,
    pub out_dir: PathBuf,
    pub js_bundle: Option<String>,
    pub css_bundle: Option<String>,
    pub bundler: &'static str,
}

pub async fn run(args: Args, _config: &Config) -> Result<()> {
    match args.command {
        Some(ServeCommand::Build { dir, out }) => {
            let root = resolve_project_dir(dir)?;
            let out_dir = resolve_out_dir(&root, &out);
            let started = std::time::Instant::now();
            let report = build_project(&root, &out_dir, true)?;
            println!(
                "{} Built {} project into {} ({}, {:.2}s)",
                "✓".green().bold(),
                kind_label(&report.kind),
                report.out_dir.display().to_string().bold(),
                report.bundler,
                started.elapsed().as_secs_f64()
            );
            if let Some(js) = &report.js_bundle {
                println!("  • JS:  /assets/{js}");
            }
            if let Some(css) = &report.css_bundle {
                println!("  • CSS: /assets/{css}");
            }
            Ok(())
        }
        Some(ServeCommand::Preview {
            dir,
            out,
            port,
            host,
            api,
        }) => {
            let root = resolve_project_dir(dir)?;
            let out_dir = resolve_out_dir(&root, &out);
            if !out_dir.join("index.html").is_file() {
                println!(
                    "{} No build found at {}; building first...",
                    "→".cyan(),
                    out_dir.display()
                );
                build_project(&root, &out_dir, true)?;
            }
            let upstream = resolve_upstream_api(api.as_deref());
            serve_http(
                ServeState {
                    serve_root: Arc::new(RwLock::new(out_dir)),
                    upstream_api: upstream.clone(),
                    dev_mode: false,
                    hmr_tx: None,
                    client: build_http_client()?,
                },
                &host,
                port,
                "preview",
                &upstream,
            )
            .await
        }
        None => {
            let root = resolve_project_dir(args.dir)?;
            let kind = detect_project(&root)?;
            let upstream = resolve_upstream_api(args.api.as_deref());
            match kind {
                ProjectKind::GoZip => serve_go_zip(&root, &args.host, args.port).await,
                ProjectKind::WebSpa { .. } => {
                    serve_web_spa_dev(&root, &args.host, args.port, &upstream).await
                }
                ProjectKind::Static => {
                    serve_static_dev(&root, &args.host, args.port, &upstream).await
                }
            }
        }
    }
}

fn resolve_project_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    let raw = match dir {
        Some(d) => d,
        None => std::env::current_dir().context("determine current working directory")?,
    };
    std::fs::canonicalize(&raw)
        .with_context(|| format!("project directory not found: {}", raw.display()))
}

fn resolve_out_dir(root: &Path, out: &Path) -> PathBuf {
    if out.is_absolute() {
        out.to_path_buf()
    } else {
        root.join(out)
    }
}

fn kind_label(kind: &ProjectKind) -> &'static str {
    match kind {
        ProjectKind::WebSpa { .. } => "Web SPA",
        ProjectKind::GoZip => "Go hanzo.ai/zip",
        ProjectKind::Static => "Static Web",
    }
}

/// Auto-detect the local project type in `dir`.
pub fn detect_project(dir: &Path) -> Result<ProjectKind> {
    let go_mod = dir.join("go.mod");
    if go_mod.is_file() {
        if let Ok(contents) = std::fs::read_to_string(&go_mod) {
            if contents.contains("hanzo.ai/zip") {
                return Ok(ProjectKind::GoZip);
            }
        }
    }

    let index_html = dir.join("index.html");
    if index_html.is_file() {
        let html = std::fs::read_to_string(&index_html)
            .with_context(|| format!("read {}", index_html.display()))?;
        if let Some(script_src) = extract_module_script_src(&html) {
            let rel = script_src.trim_start_matches('/').trim_start_matches("./");
            let rel_clean = rel.split('?').next().unwrap_or(rel);
            let candidate = dir.join(rel_clean);
            if candidate.is_file() && is_bundle_entrypoint(rel_clean) {
                return Ok(ProjectKind::WebSpa {
                    entry: PathBuf::from(rel_clean),
                });
            }
        }

        for candidate in [
            "src/main.tsx",
            "src/index.tsx",
            "src/App.tsx",
            "src/main.ts",
            "src/index.ts",
            "src/main.jsx",
            "src/index.jsx",
        ] {
            if dir.join(candidate).is_file() {
                return Ok(ProjectKind::WebSpa {
                    entry: PathBuf::from(candidate),
                });
            }
        }

        return Ok(ProjectKind::Static);
    }

    bail!(
        "could not detect a servable project in {} (expected `index.html` or `go.mod` referencing `hanzo.ai/zip`)",
        dir.display()
    )
}

fn is_bundle_entrypoint(rel: &str) -> bool {
    rel.starts_with("src/")
        || rel.ends_with(".tsx")
        || rel.ends_with(".ts")
        || rel.ends_with(".jsx")
        || rel.ends_with(".mts")
}

/// Extract the `src="..."` attribute of the first `<script ... type="module" ...>` tag in `html`.
pub fn extract_module_script_src(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(rel_idx) = lower[offset..].find("<script") {
        let start = offset + rel_idx;
        let Some(end_rel) = lower[start..].find('>') else {
            break;
        };
        let end = start + end_rel;
        let open_tag = &html[start..=end];
        let open_lower = &lower[start..=end];
        if open_lower.contains("type=\"module\"") || open_lower.contains("type='module'") {
            if let Some(src) = extract_attr(open_tag, "src") {
                if !src.starts_with("http://") && !src.starts_with("https://") {
                    return Some(src);
                }
            }
        }
        offset = end + 1;
    }
    None
}

fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle_dq = format!("{attr_name}=\"");
    if let Some(pos) = lower.find(&needle_dq) {
        let val_start = pos + needle_dq.len();
        let rest = &tag[val_start..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    let needle_sq = format!("{attr_name}='");
    if let Some(pos) = lower.find(&needle_sq) {
        let val_start = pos + needle_sq.len();
        let rest = &tag[val_start..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Determine whether a request path should proxy to `HANZO_API`.
///
/// Proxies `/v1` (`/*`), `/mcp` (`/*`), and `/api` (`/*`) while keeping SPA routes
/// like `/velocity` or `/compose` local.
pub fn is_proxy_path(path: &str) -> bool {
    has_segment_prefix(path, b"/v1")
        || has_segment_prefix(path, b"/mcp")
        || has_segment_prefix(path, b"/api")
}

fn has_segment_prefix(path: &str, prefix: &[u8]) -> bool {
    let bytes = path.as_bytes();
    bytes == prefix
        || (bytes.len() > prefix.len()
            && &bytes[..prefix.len()] == prefix
            && bytes[prefix.len()] == b'/')
}

/// Resolve the upstream Hanzo API origin (`--api`, `HANZO_API`, local `:3690`, or `https://api.hanzo.ai`).
pub fn resolve_upstream_api(explicit: Option<&str>) -> String {
    if let Some(url) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return url.trim_end_matches('/').to_string();
    }
    for var in ["HANZO_API", "VITE_HANZO_API"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.trim_end_matches('/').to_string();
            }
        }
    }
    let local_up = SocketAddr::from(([127, 0, 0, 1], 3690));
    if TcpStream::connect_timeout(&local_up, Duration::from_millis(25)).is_ok() {
        return "http://127.0.0.1:3690".to_string();
    }
    "https://api.hanzo.ai".to_string()
}

/// Rewrite `index.html` to reference compiled `/assets/...` JS & CSS bundles and optionally inject HMR.
pub fn rewrite_index_html(
    html: &str,
    js_bundle: Option<&str>,
    css_bundle: Option<&str>,
    inject_hmr: bool,
) -> String {
    let mut out = html.to_string();

    if let Some(js) = js_bundle {
        let replacement_script =
            format!("<script type=\"module\" crossorigin src=\"/assets/{js}\"></script>");
        if let Some((span_start, span_end)) = find_module_script_span(&out) {
            out.replace_range(span_start..span_end, &replacement_script);
        } else if let Some(pos) = find_case_insensitive(&out, "</body>") {
            out.insert_str(pos, &format!("  {replacement_script}\n"));
        } else {
            out.push('\n');
            out.push_str(&replacement_script);
        }
    }

    if let Some(css) = css_bundle {
        let link_tag = format!("<link rel=\"stylesheet\" crossorigin href=\"/assets/{css}\" />");
        if let Some(pos) = find_case_insensitive(&out, "</head>") {
            out.insert_str(pos, &format!("  {link_tag}\n"));
        } else {
            out = format!("{link_tag}\n{out}");
        }
    }

    if inject_hmr && !out.contains("data-hanzo-hmr") {
        if let Some(pos) = find_case_insensitive(&out, "</body>") {
            out.insert_str(pos, &format!("  {HMR_CLIENT_SNIPPET}\n"));
        } else {
            out.push('\n');
            out.push_str(HMR_CLIENT_SNIPPET);
        }
    }

    out
}

fn find_module_script_span(html: &str) -> Option<(usize, usize)> {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(rel_idx) = lower[offset..].find("<script") {
        let start = offset + rel_idx;
        let open_end = start + lower[start..].find('>')?;
        let open_lower = &lower[start..=open_end];
        let is_mod = open_lower.contains("type=\"module\"") || open_lower.contains("type='module'");
        let has_src = open_lower.contains("src=");
        if is_mod && has_src {
            if open_lower.ends_with("/>") {
                return Some((start, open_end + 1));
            }
            if let Some(close_rel) = lower[open_end + 1..].find("</script>") {
                let close_end = open_end + 1 + close_rel + "</script>".len();
                return Some((start, close_end));
            }
        }
        offset = open_end + 1;
    }
    None
}

fn find_case_insensitive(haystack: &str, needle_lower: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle_lower)
}

/// Build `root` into `out_dir`. When `production` is true, minifies and emits `404.html`;
/// when false (dev mode), emits unminified bundles and injects the live-reload SSE client.
pub fn build_project(root: &Path, out_dir: &Path, production: bool) -> Result<BuildReport> {
    let kind = detect_project(root)?;
    match &kind {
        ProjectKind::WebSpa { entry } => {
            if out_dir.exists() {
                std::fs::remove_dir_all(out_dir)
                    .with_context(|| format!("clean output directory {}", out_dir.display()))?;
            }
            let assets_dir = out_dir.join("assets");
            std::fs::create_dir_all(&assets_dir)
                .with_context(|| format!("create {}", assets_dir.display()))?;

            let public_dir = root.join("public");
            if public_dir.is_dir() {
                copy_dir_recursive(&public_dir, out_dir)?;
            }

            let (js_bundle, css_bundle, bundler) =
                if let Some(esbuild) = find_esbuild(root) {
                    let (js, css) = run_esbuild(&esbuild, root, entry, out_dir, production)?;
                    (Some(js), css, "esbuild")
                } else {
                    let (js, css) = bundle_builtin_ts(root, entry, &assets_dir)?;
                    (Some(js), css, "hanzo-builtin")
                };

            let raw_html = std::fs::read_to_string(root.join("index.html"))
                .context("read project index.html")?;
            let rendered_html = rewrite_index_html(
                &raw_html,
                js_bundle.as_deref(),
                css_bundle.as_deref(),
                !production,
            );
            std::fs::write(out_dir.join("index.html"), &rendered_html)
                .context("write built index.html")?;
            if production {
                std::fs::write(out_dir.join("404.html"), &rendered_html)
                    .context("write SPA fallback 404.html")?;
            }

            Ok(BuildReport {
                kind,
                out_dir: out_dir.to_path_buf(),
                js_bundle,
                css_bundle,
                bundler,
            })
        }
        ProjectKind::Static => {
            if out_dir != root {
                if out_dir.exists() {
                    std::fs::remove_dir_all(out_dir)?;
                }
                std::fs::create_dir_all(out_dir)?;
                copy_dir_recursive(root, out_dir)?;
                if production && out_dir.join("index.html").is_file() && !out_dir.join("404.html").exists() {
                    std::fs::copy(out_dir.join("index.html"), out_dir.join("404.html"))?;
                }
            }
            Ok(BuildReport {
                kind,
                out_dir: out_dir.to_path_buf(),
                js_bundle: None,
                css_bundle: None,
                bundler: "static-copy",
            })
        }
        ProjectKind::GoZip => {
            std::fs::create_dir_all(out_dir)?;
            let bin_name = if cfg!(windows) { "server.exe" } else { "server" };
            let bin_out = out_dir.join(bin_name);
            let status = std::process::Command::new("go")
                .current_dir(root)
                .arg("build")
                .arg("-o")
                .arg(&bin_out)
                .arg(".")
                .status()
                .context("invoke `go build` for hanzo.ai/zip project")?;
            if !status.success() {
                bail!("`go build` failed with status {status}");
            }
            Ok(BuildReport {
                kind,
                out_dir: out_dir.to_path_buf(),
                js_bundle: None,
                css_bundle: None,
                bundler: "go-build",
            })
        }
    }
}

enum EsbuildCommand {
    Binary(PathBuf),
    Npx(PathBuf),
}

fn find_esbuild(project_dir: &Path) -> Option<EsbuildCommand> {
    let bin_name = if cfg!(windows) { "esbuild.cmd" } else { "esbuild" };

    // 1. Walk `project_dir` and ancestor `node_modules/.bin/esbuild`.
    let mut cursor: Option<&Path> = Some(project_dir);
    for _ in 0..5 {
        let Some(dir) = cursor else { break };
        let candidate = dir.join("node_modules").join(".bin").join(bin_name);
        if candidate.is_file() {
            return Some(EsbuildCommand::Binary(candidate));
        }
        if let Some(pnpm_bin) = find_esbuild_in_pnpm(&dir.join("node_modules").join(".pnpm")) {
            return Some(EsbuildCommand::Binary(pnpm_bin));
        }
        cursor = dir.parent();
    }

    // 2. Check PATH (`esbuild` global or user install).
    if let Ok(found) = which::which("esbuild") {
        return Some(EsbuildCommand::Binary(found));
    }

    // 3. `npx --no-install esbuild` when `node_modules/esbuild` exists.
    if project_dir.join("node_modules").join("esbuild").is_dir() {
        if let Ok(npx) = which::which("npx") {
            return Some(EsbuildCommand::Npx(npx));
        }
    }

    None
}

fn find_esbuild_in_pnpm(pnpm_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(pnpm_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("@esbuild+") {
            let scope_dir = entry.path().join("node_modules").join("@esbuild");
            if let Ok(pkgs) = std::fs::read_dir(scope_dir) {
                for pkg in pkgs.flatten() {
                    let bin = pkg.path().join("bin").join("esbuild");
                    if bin.is_file() {
                        return Some(bin);
                    }
                }
            }
        } else if name_str.starts_with("esbuild@") {
            let bin = entry
                .path()
                .join("node_modules")
                .join("esbuild")
                .join("bin")
                .join("esbuild");
            if bin.is_file() {
                return Some(bin);
            }
        }
    }
    None
}

fn run_esbuild(
    esbuild: &EsbuildCommand,
    project_dir: &Path,
    entry: &Path,
    out_dir: &Path,
    production: bool,
) -> Result<(String, Option<String>)> {
    let assets_dir = out_dir.join("assets");
    let meta_path = out_dir.join(".hanzo-meta.json");

    let mut cmd = match esbuild {
        EsbuildCommand::Binary(bin) => std::process::Command::new(bin),
        EsbuildCommand::Npx(npx) => {
            let mut c = std::process::Command::new(npx);
            c.arg("--no-install").arg("esbuild");
            c
        }
    };

    let mode = if production { "production" } else { "development" };
    let prod_bool = if production { "true" } else { "false" };
    let dev_bool = if production { "false" } else { "true" };

    cmd.current_dir(project_dir)
        .arg(entry)
        .arg("--bundle")
        .arg("--format=esm")
        .arg("--platform=browser")
        .arg("--jsx=automatic")
        .arg("--sourcemap")
        .arg(format!("--outdir={}", assets_dir.display()))
        .arg("--public-path=/assets")
        .arg("--entry-names=[name]-[hash]")
        .arg("--asset-names=[name]-[hash]")
        .arg(format!("--metafile={}", meta_path.display()))
        .arg("--resolve-extensions=.web.tsx,.web.ts,.web.jsx,.web.js,.mjs,.js,.mts,.ts,.jsx,.tsx,.json")
        .arg("--alias:react-native=react-native-web")
        .arg("--alias:@react-native/assets-registry/registry=react-native-web/dist/modules/AssetRegistry")
        .arg("--define:global=globalThis")
        .arg("--define:process.env={}")
        .arg(format!("--define:process.env.NODE_ENV=\"{mode}\""))
        .arg("--define:process.env.GUI_TARGET=\"web\"")
        .arg("--define:import.meta.env={}")
        .arg(format!("--define:import.meta.env.MODE=\"{mode}\""))
        .arg(format!("--define:import.meta.env.PROD={prod_bool}"))
        .arg(format!("--define:import.meta.env.DEV={dev_bool}"))
        .arg("--define:import.meta.env.BASE_URL=\"/\"");

    if project_dir.join("src").is_dir() {
        cmd.arg("--alias:~=./src");
    }

    // Forward standard `VITE_*` and `HANZO_*` public client variables, defaulting core keys to `""`.
    let mut env_defines: BTreeMap<String, String> = BTreeMap::new();
    for key in [
        "VITE_HANZO_API",
        "VITE_HANZO_IAM",
        "VITE_HANZO_CLIENT_ID",
        "HANZO_API",
        "HANZO_IAM",
        "HANZO_CLIENT_ID",
    ] {
        let val = std::env::var(key).unwrap_or_default();
        env_defines.insert(key.to_string(), val);
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("VITE_") || k.starts_with("HANZO_PUBLIC_") {
            env_defines.insert(k, v);
        }
    }
    for (k, v) in env_defines {
        if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let json_val = serde_json::to_string(&v).unwrap_or_else(|_| "\"\"".to_string());
            cmd.arg(format!("--define:import.meta.env.{k}={json_val}"));
        }
    }

    for ext in [
        ".woff2", ".woff", ".ttf", ".otf", ".eot", ".svg", ".png", ".jpg", ".jpeg", ".gif",
        ".webp", ".ico", ".avif",
    ] {
        cmd.arg(format!("--loader:{ext}=file"));
    }

    if production {
        cmd.arg("--minify");
    }

    let output = cmd.output().context("execute esbuild bundler")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bundle failed:\n{}", stderr.trim());
    }

    let meta_bytes = std::fs::read(&meta_path).context("read esbuild metafile")?;
    let _ = std::fs::remove_file(&meta_path);
    let meta: serde_json::Value =
        serde_json::from_slice(&meta_bytes).context("parse esbuild metafile")?;

    let outputs = meta
        .get("outputs")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("esbuild metafile missing `outputs` map"))?;

    let mut js_file: Option<String> = None;
    let mut css_file: Option<String> = None;
    for (out_key, info) in outputs {
        if info.get("entryPoint").and_then(|v| v.as_str()).is_some() && out_key.ends_with(".js") {
            js_file = Path::new(out_key)
                .file_name()
                .map(|f| f.to_string_lossy().to_string());
            if let Some(css_key) = info.get("cssBundle").and_then(|v| v.as_str()) {
                css_file = Path::new(css_key)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string());
            }
            break;
        }
    }

    let js = js_file.ok_or_else(|| anyhow!("esbuild produced no JavaScript entry bundle"))?;
    Ok((js, css_file))
}

/// Pure-Rust zero-dependency TypeScript/TSX/CSS bundler fallback used when `esbuild`
/// is not installed in `node_modules` or `PATH` (e.g., standalone TS apps and hermetic tests).
fn bundle_builtin_ts(
    project_dir: &Path,
    entry: &Path,
    assets_dir: &Path,
) -> Result<(String, Option<String>)> {
    let mut visited = HashSet::new();
    let mut js_chunks = Vec::new();
    let mut css_chunks = Vec::new();
    let entry_abs = project_dir.join(entry);
    collect_builtin_modules(
        project_dir,
        &entry_abs,
        &mut visited,
        &mut js_chunks,
        &mut css_chunks,
    )?;

    let js_source = format!(
        "(function() {{\n{}\n}})();\n",
        js_chunks.join("\n")
    );
    let js_hash = short_sha256(js_source.as_bytes());
    let js_name = format!("main-{js_hash}.js");
    std::fs::write(assets_dir.join(&js_name), js_source.as_bytes())
        .context("write built-in JS bundle")?;

    let css_name = if css_chunks.is_empty() {
        None
    } else {
        let css_source = css_chunks.join("\n");
        let css_hash = short_sha256(css_source.as_bytes());
        let name = format!("main-{css_hash}.css");
        std::fs::write(assets_dir.join(&name), css_source.as_bytes())
            .context("write built-in CSS bundle")?;
        Some(name)
    };

    Ok((js_name, css_name))
}

fn collect_builtin_modules(
    project_dir: &Path,
    file_path: &Path,
    visited: &mut HashSet<PathBuf>,
    js_chunks: &mut Vec<String>,
    css_chunks: &mut Vec<String>,
) -> Result<()> {
    let canonical = std::fs::canonicalize(file_path)
        .with_context(|| format!("resolve module {}", file_path.display()))?;
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let raw = std::fs::read_to_string(&canonical)
        .with_context(|| format!("read {}", canonical.display()))?;

    if ext.eq_ignore_ascii_case("css") {
        css_chunks.push(raw);
        return Ok(());
    }

    let parent = canonical.parent().unwrap_or(project_dir);
    let mut body_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import type ") {
            continue;
        }
        if let Some(spec) = parse_import_specifier(trimmed) {
            if spec.starts_with('.') || spec.starts_with("~/") {
                let target_base = if let Some(rest) = spec.strip_prefix("~/") {
                    project_dir.join("src").join(rest)
                } else {
                    parent.join(&spec)
                };
                if let Some(resolved) = resolve_local_module(&target_base) {
                    collect_builtin_modules(
                        project_dir,
                        &resolved,
                        visited,
                        js_chunks,
                        css_chunks,
                    )?;
                    continue;
                }
            }
        }
        body_lines.push(strip_ts_line(line));
    }

    js_chunks.push(body_lines.join("\n"));
    Ok(())
}

fn parse_import_specifier(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("import ") {
        return None;
    }
    let quote_pos = trimmed.rfind('"').or_else(|| trimmed.rfind('\''))?;
    let quote_char = trimmed.as_bytes()[quote_pos] as char;
    let before = &trimmed[..quote_pos];
    let start_pos = before.rfind(quote_char)?;
    Some(before[start_pos + 1..].to_string())
}

fn resolve_local_module(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    for ext in ["tsx", "ts", "jsx", "js", "css", "mts", "mjs"] {
        let with_ext = base.with_extension(ext);
        if with_ext.is_file() {
            return Some(with_ext);
        }
    }
    if base.is_dir() {
        for idx in ["index.tsx", "index.ts", "index.jsx", "index.js"] {
            let candidate = base.join(idx);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Lightweight TypeScript/ESM line transform for the zero-node built-in fallback.
fn strip_ts_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.starts_with("interface ") || trimmed.starts_with("export interface ") {
        return String::new();
    }
    if (trimmed.starts_with("type ") || trimmed.starts_with("export type "))
        && trimmed.contains('=')
        && trimmed.ends_with(';')
    {
        return String::new();
    }
    let mut out = if let Some(rest) = trimmed.strip_prefix("export default ") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("export ") {
        rest.to_string()
    } else {
        line.to_string()
    };

    // Strip simple `const name: Type = ...` / `let name: Type = ...` annotations.
    for kw in ["const ", "let ", "var "] {
        if let Some(pos) = out.find(kw) {
            let after = &out[pos + kw.len()..];
            if let Some(eq_idx) = after.find('=') {
                let lhs = &after[..eq_idx];
                if let Some(colon_idx) = lhs.find(':') {
                    let ident = lhs[..colon_idx].trim();
                    if !ident.is_empty()
                        && ident
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                    {
                        out = format!(
                            "{}{kw}{ident} ={}",
                            &out[..pos],
                            &after[eq_idx + 1..]
                        );
                    }
                }
            }
        }
    }
    out
}

fn short_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(8);
    for b in &digest[..4] {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if matches!(
            name.as_ref(),
            ".git" | "node_modules" | "dist" | "target" | ".DS_Store"
        ) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ── Dev / Preview HTTP + SSE + Proxy Server ───────────────────────────────────

#[derive(Clone)]
struct ServeState {
    serve_root: Arc<RwLock<PathBuf>>,
    upstream_api: String,
    dev_mode: bool,
    hmr_tx: Option<broadcast::Sender<()>>,
    client: reqwest::Client,
}

fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .context("initialize upstream proxy HTTP client")
}

async fn serve_web_spa_dev(root: &Path, host: &str, port: u16, upstream: &str) -> Result<()> {
    let stage_parent = tempfile::TempDir::new().context("create dev build staging directory")?;
    let mut build_seq = 0u64;
    let initial_dir = stage_parent.path().join(format!("b{build_seq}"));
    let report = build_project(root, &initial_dir, false)?;
    println!(
        "{} Bundled {} with {}",
        "✓".green().bold(),
        root.display(),
        report.bundler.cyan()
    );

    let serve_root = Arc::new(RwLock::new(initial_dir));
    let (hmr_tx, _) = broadcast::channel::<()>(32);

    // Watch `src/`, `public/`, and `index.html` for changes and hot-rebuild.
    let watch_root = root.to_path_buf();
    let stage_base = stage_parent.path().to_path_buf();
    let serve_root_clone = Arc::clone(&serve_root);
    let hmr_tx_clone = hmr_tx.clone();
    tokio::spawn(async move {
        let mut last_sig = workspace_signature(&watch_root);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let next_sig = workspace_signature(&watch_root);
            if next_sig != last_sig {
                last_sig = next_sig;
                build_seq += 1;
                let next_dir = stage_base.join(format!("b{build_seq}"));
                match build_project(&watch_root, &next_dir, false) {
                    Ok(_) => {
                        let prev = {
                            let mut guard = serve_root_clone.write().await;
                            std::mem::replace(&mut *guard, next_dir)
                        };
                        let _ = std::fs::remove_dir_all(prev);
                        let _ = hmr_tx_clone.send(());
                        eprintln!("{} Rebuilt & pushed live reload", "↻".cyan());
                    }
                    Err(err) => {
                        eprintln!("{} Rebuild error: {err:#}", "!".red().bold());
                    }
                }
            }
        }
    });

    serve_http(
        ServeState {
            serve_root,
            upstream_api: upstream.to_string(),
            dev_mode: true,
            hmr_tx: Some(hmr_tx),
            client: build_http_client()?,
        },
        host,
        port,
        "dev (SPA)",
        upstream,
    )
    .await
}

async fn serve_static_dev(root: &Path, host: &str, port: u16, upstream: &str) -> Result<()> {
    let (hmr_tx, _) = broadcast::channel::<()>(32);
    let watch_root = root.to_path_buf();
    let hmr_tx_clone = hmr_tx.clone();
    tokio::spawn(async move {
        let mut last_sig = workspace_signature(&watch_root);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let next_sig = workspace_signature(&watch_root);
            if next_sig != last_sig {
                last_sig = next_sig;
                let _ = hmr_tx_clone.send(());
                eprintln!("{} Static file updated — reloaded browser", "↻".cyan());
            }
        }
    });

    serve_http(
        ServeState {
            serve_root: Arc::new(RwLock::new(root.to_path_buf())),
            upstream_api: upstream.to_string(),
            dev_mode: true,
            hmr_tx: Some(hmr_tx),
            client: build_http_client()?,
        },
        host,
        port,
        "dev (static)",
        upstream,
    )
    .await
}

async fn serve_go_zip(root: &Path, host: &str, port: u16) -> Result<()> {
    let base = format!("http://{host}:{port}");
    println!(
        "{} {} ({})",
        "Hanzo Serve".bold().cyan(),
        "Go hanzo.ai/zip".bold(),
        root.display()
    );
    println!("  • Server:   {}", base.bold().green());
    println!("  • Docs:     {base}/docs  ({base}/docs/llms.txt)");
    println!("  • OpenAPI:  {base}/.well-known/openapi.json");
    println!("  • MCP:      {base}/mcp");

    let addr = format!("{host}:{port}");
    let mut last_sig = go_workspace_signature(root);
    loop {
        let mut child = tokio::process::Command::new("go")
            .current_dir(root)
            .arg("run")
            .arg(".")
            .env("PORT", port.to_string())
            .env("ADDR", &addr)
            .spawn()
            .context("spawn `go run .` for hanzo.ai/zip service")?;

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    let _ = child.kill().await;
                    return Ok(());
                }
                status = child.wait() => {
                    let st = status?;
                    if !st.success() {
                        eprintln!("{} `go run .` exited ({st}); waiting for .go change...", "!".yellow());
                    }
                    while go_workspace_signature(root) == last_sig {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => return Ok(()),
                            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                        }
                    }
                    last_sig = go_workspace_signature(root);
                    break;
                }
                _ = tokio::time::sleep(Duration::from_millis(300)) => {
                    let next_sig = go_workspace_signature(root);
                    if next_sig != last_sig {
                        last_sig = next_sig;
                        eprintln!("{} Go source changed — restarting `go run .`...", "↻".cyan());
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        break;
                    }
                }
            }
        }
    }
}

async fn serve_http(
    state: ServeState,
    host: &str,
    port: u16,
    mode_label: &str,
    upstream: &str,
) -> Result<()> {
    let listener = TcpListener::bind((host, port))
        .await
        .with_context(|| format!("bind local server on {host}:{port}"))?;
    let local_addr = listener.local_addr()?;
    println!(
        "{} [{}] listening on {}",
        "Hanzo Serve".bold().cyan(),
        mode_label,
        format!("http://{local_addr}").bold().green()
    );
    println!("  • API proxy (/v1, /mcp, /api) → {}", upstream.cyan());
    if state.dev_mode {
        println!("  • Live reload SSE             → {HMR_ENDPOINT}");
    }

    serve_listener(listener, state).await
}

async fn serve_listener(listener: TcpListener, state: ServeState) -> Result<()> {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let conn_state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, conn_state).await;
                });
            }
        }
    }
}

struct ParsedRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn read_http_request(stream: &mut TokioStream) -> Result<Option<ParsedRequest>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            bail!("HTTP request header exceeds 64 KiB");
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            bail!("unexpected EOF reading HTTP headers");
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head_str = std::str::from_utf8(&buf[..header_end]).context("invalid UTF-8 in HTTP head")?;
    let mut lines = head_str.split("\r\n");
    let req_line = lines.next().ok_or_else(|| anyhow!("empty request line"))?;
    let mut parts = req_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request target"))?
        .to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, val)) = line.split_once(':') {
            let k = name.trim().to_string();
            let v = val.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse::<usize>().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    let body_start = header_end + 4;
    let mut body = Vec::with_capacity(content_length);
    if buf.len() > body_start {
        body.extend_from_slice(&buf[body_start..]);
    }
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    if body.len() > content_length {
        body.truncate(content_length);
    }

    Ok(Some(ParsedRequest {
        method,
        target,
        headers,
        body,
    }))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn handle_connection(mut stream: TokioStream, state: ServeState) -> Result<()> {
    let Some(req) = read_http_request(&mut stream).await? else {
        return Ok(());
    };

    let path = req.target.split('?').next().unwrap_or("/");

    // 1. Live-reload SSE endpoint in dev mode.
    if state.dev_mode && path == HMR_ENDPOINT {
        if let Some(tx) = &state.hmr_tx {
            let mut rx = tx.subscribe();
            let head = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: keep-alive\r\n",
                "Access-Control-Allow-Origin: *\r\n\r\n",
                ": connected\n\n"
            );
            stream.write_all(head.as_bytes()).await?;
            stream.flush().await?;
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Ok(()) => {
                                if stream.write_all(b"data: reload\n\n").await.is_err() {
                                    break;
                                }
                                let _ = stream.flush().await;
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let _ = stream.write_all(b"data: reload\n\n").await;
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {
                        if stream.write_all(b": keepalive\n\n").await.is_err() {
                            break;
                        }
                    }
                }
            }
            return Ok(());
        }
    }

    // 2. Upstream API / MCP reverse proxy (`/v1/*`, `/mcp`, `/api/*`).
    if is_proxy_path(path) {
        return proxy_to_upstream(&mut stream, &state, &req).await;
    }

    // 3. Static asset & SPA history fallback (`/* -> index.html`).
    let root = state.serve_root.read().await.clone();
    serve_static_or_spa(&mut stream, &root, path, &req.method, state.dev_mode).await
}

async fn proxy_to_upstream(
    stream: &mut TokioStream,
    state: &ServeState,
    req: &ParsedRequest,
) -> Result<()> {
    let url = format!("{}{}", state.upstream_api, req.target);
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut builder = state.client.request(method, &url);

    for (k, v) in &req.headers {
        let lower = k.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding" | "accept-encoding"
        ) {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    if !req.body.is_empty() {
        builder = builder.body(req.body.clone());
    }

    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let status_code = status.as_u16();
            let reason = status.canonical_reason().unwrap_or("OK");
            let mut resp_headers = Vec::new();
            for (name, value) in resp.headers() {
                let lower = name.as_str().to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "connection" | "transfer-encoding" | "content-encoding" | "content-length"
                ) {
                    continue;
                }
                if let Ok(v_str) = value.to_str() {
                    resp_headers.push((name.as_str().to_string(), v_str.to_string()));
                }
            }
            let body = resp.bytes().await.unwrap_or_default();
            write_http_response(
                stream,
                status_code,
                reason,
                &resp_headers,
                if req.method == "HEAD" { &[] } else { &body },
                Some(body.len()),
            )
            .await
        }
        Err(err) => {
            let payload = serde_json::json!({
                "error": "upstream_unreachable",
                "upstream": state.upstream_api,
                "message": err.to_string(),
            })
            .to_string();
            write_http_response(
                stream,
                502,
                "Bad Gateway",
                &[(
                    "Content-Type".to_string(),
                    "application/json; charset=utf-8".to_string(),
                )],
                payload.as_bytes(),
                None,
            )
            .await
        }
    }
}

async fn serve_static_or_spa(
    stream: &mut TokioStream,
    root: &Path,
    raw_path: &str,
    method: &str,
    dev_mode: bool,
) -> Result<()> {
    if method != "GET" && method != "HEAD" {
        return write_http_response(
            stream,
            405,
            "Method Not Allowed",
            &[("Content-Type".to_string(), "text/plain; charset=utf-8".to_string())],
            b"Method Not Allowed",
            None,
        )
        .await;
    }

    let Some(clean_rel) = sanitize_rel_path(raw_path) else {
        return write_http_response(
            stream,
            400,
            "Bad Request",
            &[("Content-Type".to_string(), "text/plain; charset=utf-8".to_string())],
            b"Bad Request",
            None,
        )
        .await;
    };

    let candidate = if clean_rel.as_os_str().is_empty() {
        root.join("index.html")
    } else {
        let direct = root.join(&clean_rel);
        if direct.is_dir() {
            direct.join("index.html")
        } else {
            direct
        }
    };

    let (target_file, is_immutable_asset) = if candidate.is_file() {
        let immutable = !dev_mode && raw_path.starts_with("/assets/");
        (Some(candidate), immutable)
    } else if should_spa_fallback(raw_path) && root.join("index.html").is_file() {
        (Some(root.join("index.html")), false)
    } else {
        (None, false)
    };

    let Some(file_path) = target_file else {
        return write_http_response(
            stream,
            404,
            "Not Found",
            &[("Content-Type".to_string(), "text/plain; charset=utf-8".to_string())],
            b"Not Found",
            None,
        )
        .await;
    };

    let mut bytes = std::fs::read(&file_path)
        .with_context(|| format!("read served file {}", file_path.display()))?;
    let mime = content_type_for(&file_path);

    // Static dev mode serves from the source directory directly, so inject HMR on the fly.
    if dev_mode
        && mime.starts_with("text/html")
        && !std::str::from_utf8(&bytes)
            .map(|s| s.contains("data-hanzo-hmr"))
            .unwrap_or(true)
    {
        if let Ok(html_str) = std::str::from_utf8(&bytes) {
            bytes = rewrite_index_html(html_str, None, None, true).into_bytes();
        }
    }

    let cache = if is_immutable_asset {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let headers = vec![
        ("Content-Type".to_string(), mime.to_string()),
        ("Cache-Control".to_string(), cache.to_string()),
    ];
    let total_len = bytes.len();
    write_http_response(
        stream,
        200,
        "OK",
        &headers,
        if method == "HEAD" { &[] } else { &bytes },
        Some(total_len),
    )
    .await
}

fn sanitize_rel_path(raw_path: &str) -> Option<PathBuf> {
    let trimmed = raw_path.trim_start_matches('/');
    let mut out = PathBuf::new();
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains('\0') || seg.contains('\\') {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

fn should_spa_fallback(raw_path: &str) -> bool {
    if raw_path.starts_with("/assets/") {
        return false;
    }
    match Path::new(raw_path).extension().and_then(|e| e.to_str()) {
        None => true,
        Some(ext) => ext.eq_ignore_ascii_case("html"),
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn write_http_response(
    stream: &mut TokioStream,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    body: &[u8],
    explicit_len: Option<usize>,
) -> Result<()> {
    let len = explicit_len.unwrap_or(body.len());
    let mut head = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.flush().await?;
    Ok(())
}

// ── File Watcher Signatures ───────────────────────────────────────────────────

fn workspace_signature(root: &Path) -> u64 {
    let mut hasher = Sha256::new();
    let index_html = root.join("index.html");
    hash_file_meta(&index_html, &mut hasher);
    for sub in ["src", "public"] {
        let dir = root.join(sub);
        if dir.is_dir() {
            hash_dir_meta(&dir, &mut hasher, None);
        }
    }
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

fn go_workspace_signature(root: &Path) -> u64 {
    let mut hasher = Sha256::new();
    hash_dir_meta(root, &mut hasher, Some(&["go", "mod", "sum"]));
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

fn hash_dir_meta(dir: &Path, hasher: &mut Sha256, exts: Option<&[&str]>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if matches!(
            name,
            ".git" | "node_modules" | "dist" | "target" | ".next" | "vendor"
        ) {
            continue;
        }
        if p.is_dir() {
            hash_dir_meta(&p, hasher, exts);
        } else if p.is_file() {
            if let Some(allowed) = exts {
                let ok = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| allowed.contains(&e));
                if !ok {
                    continue;
                }
            }
            hash_file_meta(&p, hasher);
        }
    }
}

fn hash_file_meta(path: &Path, hasher: &mut Sha256) {
    if let Ok(meta) = std::fs::metadata(path) {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                hasher.update(dur.as_nanos().to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn detects_web_spa_go_zip_and_static_projects() {
        let spa = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(spa.path().join("src")).unwrap();
        std::fs::write(
            spa.path().join("index.html"),
            r#"<!doctype html><html><body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body></html>"#,
        )
        .unwrap();
        std::fs::write(spa.path().join("src/main.tsx"), "console.log('ok')\n").unwrap();
        assert_eq!(
            detect_project(spa.path()).unwrap(),
            ProjectKind::WebSpa {
                entry: PathBuf::from("src/main.tsx")
            }
        );

        let go_zip = tempfile::tempdir().unwrap();
        std::fs::write(
            go_zip.path().join("go.mod"),
            "module example.com/demo\n\ngo 1.26.3\n\nrequire hanzo.ai/zip v0.5.1\n",
        )
        .unwrap();
        assert_eq!(detect_project(go_zip.path()).unwrap(), ProjectKind::GoZip);

        let static_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            static_dir.path().join("index.html"),
            "<!doctype html><h1>Hello</h1>",
        )
        .unwrap();
        assert_eq!(
            detect_project(static_dir.path()).unwrap(),
            ProjectKind::Static
        );
    }

    #[test]
    fn rewrites_index_html_with_assets_and_hmr() {
        let src = r#"<!doctype html>
<html>
  <head>
    <title>Test</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>"#;

        let dev_html = rewrite_index_html(src, Some("main-ABCD.js"), Some("main-1234.css"), true);
        assert!(dev_html.contains("/assets/main-ABCD.js"));
        assert!(dev_html.contains("/assets/main-1234.css"));
        assert!(!dev_html.contains("/src/main.tsx"));
        assert!(dev_html.contains("/__hanzo_hmr"));

        let prod_html =
            rewrite_index_html(src, Some("main-ABCD.js"), Some("main-1234.css"), false);
        assert!(prod_html.contains("/assets/main-ABCD.js"));
        assert!(prod_html.contains("/assets/main-1234.css"));
        assert!(!prod_html.contains("/__hanzo_hmr"));
    }

    #[test]
    fn routes_proxy_paths_without_matching_spa_routes() {
        assert!(is_proxy_path("/v1"));
        assert!(is_proxy_path("/v1/social/posts"));
        assert!(is_proxy_path("/mcp"));
        assert!(is_proxy_path("/api/health"));

        assert!(!is_proxy_path("/"));
        assert!(!is_proxy_path("/velocity"));
        assert!(!is_proxy_path("/compose"));
        assert!(!is_proxy_path("/assets/main-1234.js"));
        assert!(!is_proxy_path("/mcp-guide"));
    }

    #[test]
    fn builds_spa_project_hermetically_with_zero_vite() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join("src")).unwrap();
        std::fs::create_dir_all(proj.path().join("public")).unwrap();
        std::fs::write(proj.path().join("public/robots.txt"), "User-agent: *\n").unwrap();
        std::fs::write(
            proj.path().join("index.html"),
            r#"<!doctype html><html><head><title>Hermetic</title></head><body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            proj.path().join("src/theme.css"),
            "body { background: #000; color: #fff; }\n",
        )
        .unwrap();
        std::fs::write(
            proj.path().join("src/util.ts"),
            "export interface Msg { text: string }\nexport const label: string = 'hanzo-serve-ok';\n",
        )
        .unwrap();
        std::fs::write(
            proj.path().join("src/main.tsx"),
            "import './theme.css';\nimport { label } from './util';\ndocument.body.dataset.ready = label;\n",
        )
        .unwrap();

        let out = proj.path().join("dist");
        let report = build_project(proj.path(), &out, true).unwrap();
        assert!(out.join("index.html").is_file());
        assert!(out.join("404.html").is_file());
        assert!(out.join("robots.txt").is_file());

        let js_name = report.js_bundle.expect("emits JS bundle");
        let css_name = report.css_bundle.expect("emits CSS bundle");
        assert!(out.join("assets").join(&js_name).is_file());
        assert!(out.join("assets").join(&css_name).is_file());

        let built_html = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(built_html.contains(&format!("/assets/{js_name}")));
        assert!(built_html.contains(&format!("/assets/{css_name}")));
    }

    #[tokio::test]
    async fn serves_spa_fallback_and_proxies_v1_to_upstream() {
        // Spin up a tiny mock `/v1` upstream server.
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = upstream_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    if let Ok(Some(req)) = read_http_request(&mut sock).await {
                        let auth = req
                            .headers
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        let body = serde_json::json!({
                            "target": req.target,
                            "auth": auth,
                        })
                        .to_string();
                        let _ = write_http_response(
                            &mut sock,
                            200,
                            "OK",
                            &[(
                                "Content-Type".to_string(),
                                "application/json".to_string(),
                            )],
                            body.as_bytes(),
                            None,
                        )
                        .await;
                    }
                });
            }
        });

        let dist = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dist.path().join("assets")).unwrap();
        std::fs::write(dist.path().join("index.html"), "<!doctype html><title>SPA</title>").unwrap();
        std::fs::write(dist.path().join("assets/app.js"), "console.log(1);").unwrap();

        let app_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let app_addr = app_listener.local_addr().unwrap();
        let state = ServeState {
            serve_root: Arc::new(RwLock::new(dist.path().to_path_buf())),
            upstream_api: format!("http://{upstream_addr}"),
            dev_mode: false,
            hmr_tx: None,
            client: build_http_client().unwrap(),
        };
        tokio::spawn(async move {
            let _ = serve_listener(app_listener, state).await;
        });

        let client = reqwest::Client::new();
        // SPA client route falls back to index.html.
        let spa_res = client
            .get(format!("http://{app_addr}/compose"))
            .send()
            .await
            .unwrap();
        assert_eq!(spa_res.status(), 200);
        assert!(spa_res.text().await.unwrap().contains("<title>SPA</title>"));

        // Missing `/assets/*` returns 404 rather than HTML.
        let miss_res = client
            .get(format!("http://{app_addr}/assets/missing.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(miss_res.status(), 404);

        // `/v1/*` proxies to upstream preserving Authorization and query string.
        let api_res = client
            .get(format!("http://{app_addr}/v1/social/posts?status=published"))
            .header("Authorization", "Bearer tok_123")
            .send()
            .await
            .unwrap();
        assert_eq!(api_res.status(), 200);
        let json: serde_json::Value = api_res.json().await.unwrap();
        assert_eq!(json["target"], "/v1/social/posts?status=published");
        assert_eq!(json["auth"], "Bearer tok_123");
    }

    #[test]
    fn cli_grammar_parses_serve_dev_build_and_preview() {
        let cmd = crate::commands::product::augment(crate::Cli::command());
        for argv in [
            vec!["hanzo", "serve"],
            vec!["hanzo", "serve", "--port", "3300"],
            vec!["hanzo", "serve", "--port", "3300", "--api", "https://api.hanzo.ai", "./app"],
            vec!["hanzo", "serve", "build"],
            vec!["hanzo", "serve", "build", "./app", "--out", "dist"],
            vec!["hanzo", "serve", "preview", "--port", "3300"],
        ] {
            cmd.clone()
                .try_get_matches_from(&argv)
                .unwrap_or_else(|e| panic!("failed to parse {argv:?}: {e}"));
        }
    }
}
