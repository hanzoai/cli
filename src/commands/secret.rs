//! `hanzo secret scan PATH` — find exposed credentials and private keys in LOCAL
//! files before they leave your machine.
//!
//! This is a LOCAL scanner, deliberately distinct from any cloud secret service:
//! `kms`/`connector` STORE secrets in Hanzo KMS; this READS your working tree and
//! flags secrets that should never have been written there. It reads only the
//! files you point it at, sends nothing anywhere, and exits NON-ZERO when it finds
//! anything — so `hanzo secret scan .` drops straight into a pre-commit hook or CI.
//!
//! Detection is dependency-free and structural: known credential prefixes with a
//! minimum length (`sk-`, `sk-ant-`, `hk-`, `ghp_`, GitLab/Slack/Stripe…), the AWS
//! access-key and Google-API-key shapes, PEM PRIVATE KEY headers, JWTs, and a
//! high-entropy value assigned to a secret-named field. Findings are REDACTED —
//! the tool that hunts for leaked secrets must never print one in full.

use anyhow::{bail, Result};
use colored::*;
use std::path::{Path, PathBuf};

/// One finding: where, which rule, and the REDACTED evidence.
struct Finding {
    file: PathBuf,
    line: usize,
    rule: &'static str,
    redacted: String,
}

/// Directories that never hold source-of-truth secrets and only add noise + time.
const SKIP_DIRS: &[&str] =
    &[".git", "node_modules", "target", "dist", "build", "vendor", ".venv", "__pycache__", ".next"];

/// Files larger than this are treated as data, not code, and skipped.
const MAX_FILE: u64 = 5 * 1024 * 1024;

/// `hanzo secret scan PATH` — scan a file or a directory tree.
pub async fn scan(path: PathBuf) -> Result<()> {
    if !path.exists() {
        bail!("no such path: {}", path.display());
    }
    let mut findings = Vec::new();
    let mut files = 0usize;
    walk(&path, &mut files, &mut findings);

    for f in &findings {
        println!(
            "{}:{}: {} {}",
            f.file.display(),
            f.line,
            format!("[{}]", f.rule).yellow(),
            f.redacted.dimmed()
        );
    }

    if findings.is_empty() {
        println!("{} scanned {} file(s) — no exposed secrets", "✓".green(), files);
        return Ok(());
    }
    bail!(
        "{} potential secret(s) in {} file(s) — rotate anything real and remove it from the tree",
        findings.len(),
        findings.iter().map(|f| &f.file).collect::<std::collections::BTreeSet<_>>().len()
    )
}

/// Recursive, symlink-safe walk: descend real directories, scan real files.
fn walk(path: &Path, files: &mut usize, out: &mut Vec<Finding>) {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.file_type().is_symlink() {
        return; // never follow a symlink — no loops, no escaping the named tree
    }
    if meta.is_dir() {
        if path.file_name().and_then(|n| n.to_str()).map(|n| SKIP_DIRS.contains(&n)).unwrap_or(false) {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut kids: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            kids.sort();
            for k in kids {
                walk(&k, files, out);
            }
        }
        return;
    }
    if !meta.is_file() || meta.len() > MAX_FILE {
        return;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    // A NUL in the first chunk means binary — don't scan (and don't print bytes).
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    *files += 1;
    for (i, line) in text.lines().enumerate() {
        for (rule, hit) in scan_line(line) {
            out.push(Finding { file: path.to_path_buf(), line: i + 1, rule, redacted: redact(&hit) });
        }
    }
}

/// All rule hits on one line: `(rule, matched-substring)`.
fn scan_line(line: &str) -> Vec<(&'static str, String)> {
    let mut hits = Vec::new();

    // PEM private keys — a whole-line structural marker.
    if line.contains("PRIVATE KEY-----") && line.contains("-----BEGIN ") {
        hits.push(("private-key", line.trim().to_string()));
    }

    for tok in tokens(line) {
        if let Some(rule) = prefixed(tok) {
            hits.push((rule, tok.to_string()));
        } else if is_aws_key(tok) {
            hits.push(("aws-access-key", tok.to_string()));
        } else if is_google_key(tok) {
            hits.push(("google-api-key", tok.to_string()));
        } else if is_jwt(tok) {
            hits.push(("jwt", tok.to_string()));
        }
    }

    // A high-entropy value assigned to a secret-named field — catches keys with no
    // recognizable prefix (`api_secret = "9f3c…"`), without flooding on prose.
    if let Some(v) = entropy_assignment(line) {
        if !hits.iter().any(|(_, m)| m == &v) {
            hits.push(("high-entropy-secret", v));
        }
    }
    hits
}

/// Split a line into credential-shaped tokens (the set a key/token draws from).
/// `=` is a SEPARATOR, not a token char, so `OPENAI_API_KEY=sk-…` splits the name
/// from the value (base64 `=` padding is dropped — detection needs the prefix +
/// length, not the tail).
fn tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || "-_./+".contains(c)))
        .filter(|t| t.len() >= 8)
}

/// A known credential prefix with a plausible length → its rule name.
fn prefixed(tok: &str) -> Option<&'static str> {
    // (prefix, min total length, rule). `sk-ant-` before `sk-` — most specific first.
    const RULES: &[(&str, usize, &str)] = &[
        ("sk-ant-", 24, "anthropic-key"),
        ("sk-", 20, "openai-key"),
        ("hk-", 16, "hanzo-key"),
        ("github_pat_", 24, "github-token"),
        ("ghp_", 36, "github-token"),
        ("gho_", 36, "github-token"),
        ("ghu_", 36, "github-token"),
        ("ghs_", 36, "github-token"),
        ("ghr_", 36, "github-token"),
        ("glpat-", 24, "gitlab-token"),
        ("xoxb-", 24, "slack-token"),
        ("xoxp-", 24, "slack-token"),
        ("xoxa-", 24, "slack-token"),
        ("xoxr-", 24, "slack-token"),
        ("sk_live_", 24, "stripe-key"),
        ("rk_live_", 24, "stripe-key"),
        ("AKIA", 20, "aws-access-key"),
    ];
    RULES
        .iter()
        .find(|(p, min, _)| tok.len() >= *min && tok.starts_with(p))
        .map(|(_, _, rule)| *rule)
}

/// AWS access key id: `AKIA` + 16 uppercase alphanumerics, exactly.
fn is_aws_key(tok: &str) -> bool {
    tok.len() == 20
        && tok.starts_with("AKIA")
        && tok[4..].bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Google API key: `AIza` + 35 URL-safe chars.
fn is_google_key(tok: &str) -> bool {
    tok.len() == 39
        && tok.starts_with("AIza")
        && tok[4..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A JWT: three base64url segments, the first a `{"…` header (`eyJ`).
fn is_jwt(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|p| p.len() >= 8 && p.bytes().all(is_b64url))
}

fn is_b64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'='
}

/// A high-entropy value assigned to a SECRET-named field on this line. We only
/// look when the field name itself signals a secret, so a long hash or a base64
/// asset in ordinary code does not trip the scanner.
fn entropy_assignment(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let named = ["secret", "token", "password", "passwd", "pwd", "apikey", "api_key", "api-key", "access_key"]
        .iter()
        .any(|k| lower.contains(k));
    if !named {
        return None;
    }
    // The value after the last `=` or `:` on the line, unquoted.
    let rhs = line.rsplit(['=', ':']).next()?.trim().trim_matches(['"', '\'', ',', ';'].as_ref());
    let val = rhs.trim();
    if val.len() >= 16 && val.len() <= 200 && looks_random(val) && entropy(val) >= 3.5 {
        Some(val.to_string())
    } else {
        None
    }
}

/// Drawn from a key alphabet (base64/hex-ish) — excludes sentences with spaces.
fn looks_random(s: &str) -> bool {
    !s.contains(' ') && s.bytes().all(|b| b.is_ascii_alphanumeric() || "-_./+=".contains(b as char))
}

/// Shannon entropy (bits/char) — a proxy for randomness.
fn entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Redact a matched secret: keep enough to recognize it, never enough to use it.
fn redact(s: &str) -> String {
    let s = s.trim();
    if s.contains("PRIVATE KEY-----") {
        return "-----BEGIN … PRIVATE KEY----- (redacted)".to_string();
    }
    let n = s.chars().count();
    if n <= 8 {
        return "*".repeat(n);
    }
    let head: String = s.chars().take(4).collect();
    let tail: String = s.chars().skip(n - 2).collect();
    format!("{head}…{tail} ({n} chars)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(line: &str) -> Vec<&'static str> {
        scan_line(line).into_iter().map(|(r, _)| r).collect()
    }

    #[test]
    fn detects_provider_keys() {
        assert!(rules("OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwx").contains(&"openai-key"));
        assert!(rules("key: sk-ant-api03-abcdefghijklmnopqrst").contains(&"anthropic-key"));
        assert!(rules("HANZO=hk-abcdefghijklmnop").contains(&"hanzo-key"));
        assert!(rules("token ghp_0123456789012345678901234567890123abcd").contains(&"github-token"));
    }

    #[test]
    fn detects_structural_shapes() {
        assert!(rules("aws = AKIAIOSFODNN7EXAMPLE").contains(&"aws-access-key"));
        assert!(rules("g AIza01234567890123456789012345678901234").contains(&"google-api-key"));
        assert!(rules("-----BEGIN RSA PRIVATE KEY-----").contains(&"private-key"));
        assert!(rules("eyJhbGciOi.eyJzdWIiOm.SflKxwRJSM").contains(&"jwt"));
    }

    #[test]
    fn high_entropy_only_when_named_a_secret() {
        assert!(rules("api_secret = \"9f3ck2Lm8Qz1Yx7Rt4Wb0Nv6Hs5Pd\"").contains(&"high-entropy-secret"));
        // The same value under a non-secret name is NOT flagged.
        assert!(rules("commit = \"9f3ck2Lm8Qz1Yx7Rt4Wb0Nv6Hs5Pd\"").is_empty());
    }

    #[test]
    fn ignores_ordinary_prose_and_short_tokens() {
        assert!(rules("the quick brown fox jumps over the lazy dog").is_empty());
        assert!(rules("let x = 42; // a comment about tokens").is_empty());
    }

    #[test]
    fn redaction_never_prints_the_whole_secret() {
        let r = redact("sk-abcdefghijklmnopqrstuvwx");
        assert!(!r.contains("abcdefghijklmnop"));
        assert!(r.starts_with("sk-a"));
    }
}
