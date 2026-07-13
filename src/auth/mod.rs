// auth — native Hanzo IAM authentication for the `hanzo auth` group. Login runs
// the RFC 8628 device flow (link + terminal QR + code), or stores a pasted
// token with `--api-key`. Identity and expiry are read from the token's own
// claims. Credentials live in ~/.hanzo/credentials.json (mode 0600), the same
// store the Go CLI and the GPU daemon read. No Python, no shell-out.

mod device;
mod jwt;
mod store;

use anyhow::{bail, Result};
use colored::*;

use crate::AuthCommands;
use device::{poll_device_token, Iam, TokenResp, CLIENT_ID, IAM_ISSUER, SCOPE};
use store::Credentials;

pub async fn run(command: AuthCommands) -> Result<()> {
    match command {
        AuthCommands::Login { email, api_key } => login(email, api_key).await,
        AuthCommands::Logout => {
            store::logout()?;
            println!("Logged out.");
            Ok(())
        }
        AuthCommands::Whoami => whoami(),
        AuthCommands::Status => status(),
    }
}

/// The IAM issuer, overridable with HANZO_IAM_ISSUER (used by tests/staging).
fn issuer() -> String {
    non_empty_env("HANZO_IAM_ISSUER").unwrap_or_else(|| IAM_ISSUER.to_string())
}

/// The OAuth client id, overridable with HANZO_CLIENT_ID.
fn client_id() -> String {
    non_empty_env("HANZO_CLIENT_ID").unwrap_or_else(|| CLIENT_ID.to_string())
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

async fn login(email: Option<String>, api_key: Option<String>) -> Result<()> {
    let mut creds = match api_key {
        // Paste an externally-minted token — decode claims for identity if it is
        // a JWT; otherwise fall back to --email as the subject.
        Some(token) => {
            let mut c = creds_from_token(&TokenResp {
                access_token: token,
                token_type: "Bearer".into(),
                ..Default::default()
            });
            if c.subject.is_empty() {
                if let Some(e) = &email {
                    c.subject = e.clone();
                }
            }
            c
        }
        // The ONE interactive way: RFC 8628 device flow, headless-safe.
        None => {
            let iam = Iam::new(&issuer(), &client_id());
            run_device_login(&iam, SCOPE).await?
        }
    };

    // Preserve any keys this flow does not own (platform_token, …).
    creds.extra = Credentials::load()?.extra;
    creds.save()?;

    let who = identity(&creds);
    println!(
        "Logged in as {} (token expires {})",
        who.bold(),
        short_time(creds.expiry)
    );
    Ok(())
}

/// Drive the interactive device sign-in and return the resulting credentials.
async fn run_device_login(iam: &Iam, scope: &str) -> Result<Credentials> {
    let da = iam.device_auth(scope).await?;
    let link = if da.verification_uri_complete.is_empty() {
        &da.verification_uri
    } else {
        &da.verification_uri_complete
    };
    println!("\nSign in on any device:\n");
    print_qr(link);
    println!("\n  {}", link.cyan());
    if !da.verification_uri.is_empty() && !da.user_code.is_empty() {
        println!(
            "  or open {} and enter code {}",
            da.verification_uri,
            da.user_code.bold()
        );
    }
    println!("\nWaiting for approval…");
    let tr = poll_device_token(iam, &da).await?;
    Ok(creds_from_token(&tr))
}

/// Render a scannable QR of the verification link; skip silently if the link
/// cannot be encoded (never block sign-in on the QR).
fn print_qr(link: &str) {
    if let Ok(qr) = qr2term::generate_qr_string(link) {
        print!("{qr}");
    }
}

/// Build Credentials from a token response, decoding identity + expiry from the
/// JWT claims. `expires_in` (token endpoint) wins for expiry; otherwise the JWT
/// `exp` claim is used.
fn creds_from_token(tr: &TokenResp) -> Credentials {
    let mut c = Credentials {
        access_token: tr.access_token.clone(),
        refresh_token: tr.refresh_token.clone(),
        token_type: if tr.token_type.is_empty() {
            "Bearer".into()
        } else {
            tr.token_type.clone()
        },
        ..Default::default()
    };
    if let Ok(claims) = jwt::decode_claims(&tr.access_token) {
        let email = jwt::claim_str(&claims, "email");
        c.subject = if email.is_empty() {
            jwt::claim_str(&claims, "sub")
        } else {
            email
        };
        c.owner = jwt::claim_str(&claims, "owner");
        let exp = jwt::claim_i64(&claims, "exp");
        if exp > 0 {
            c.expiry = exp;
        }
    }
    if tr.expires_in > 0 {
        c.expiry = now_unix() + tr.expires_in;
    }
    c
}

fn whoami() -> Result<()> {
    let creds = Credentials::load()?;
    if !creds.logged_in() {
        bail!("not logged in: run `hanzo login`");
    }
    match jwt::decode_claims(&creds.access_token) {
        Ok(claims) => {
            field("email", &jwt::claim_str(&claims, "email"));
            let mut name = jwt::claim_str(&claims, "displayName");
            if name.is_empty() {
                name = jwt::claim_str(&claims, "name");
            }
            field("name", &name);
            field("org", &jwt::claim_str(&claims, "owner"));
            field("subject", &jwt::claim_str(&claims, "sub"));
            field("issuer", &jwt::claim_str(&claims, "iss"));
            let exp = jwt::claim_i64(&claims, "exp");
            field(
                "expires",
                &short_time(if exp > 0 { exp } else { creds.expiry }),
            );
        }
        // Opaque (non-JWT) token: show what the store recorded.
        Err(_) => {
            field("subject", &creds.subject);
            field("org", &creds.owner);
            field("expires", &short_time(creds.expiry));
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let creds = Credentials::load()?;
    if !creds.logged_in() {
        println!("Not logged in. Run `hanzo login`.");
        return Ok(());
    }
    println!("Logged in as {}", identity(&creds).bold());
    if creds.expiry > 0 {
        let expired = creds.expiry <= now_unix();
        let state = if expired {
            "expired".red()
        } else {
            "valid".green()
        };
        println!("Token {} (expires {})", state, short_time(creds.expiry));
    }
    Ok(())
}

/// "subject @ owner", or "(unknown)" when the subject is missing.
fn identity(creds: &Credentials) -> String {
    let who = if creds.subject.is_empty() {
        "(unknown)".to_string()
    } else {
        creds.subject.clone()
    };
    if creds.owner.is_empty() {
        who
    } else {
        format!("{who} @ {}", creds.owner)
    }
}

/// Print a labelled field, skipping empty values to keep output tidy.
fn field(label: &str, value: &str) {
    if !value.is_empty() {
        println!("{label:<8} {value}");
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Render a unix timestamp as RFC 3339; "unknown" for zero/invalid.
fn short_time(unix: i64) -> String {
    if unix == 0 {
        return "unknown".to_string();
    }
    match chrono::DateTime::from_timestamp(unix, 0) {
        Some(dt) => dt.to_rfc3339(),
        None => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(claims: serde_json::Value) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("{hdr}.{payload}.sig")
    }

    #[test]
    fn creds_from_token_expires_in_wins_over_exp() {
        let tok = make_jwt(
            serde_json::json!({"email": "z@hanzo.ai", "owner": "hanzo", "exp": 2_000_000_000i64}),
        );
        let c = creds_from_token(&TokenResp {
            access_token: tok,
            refresh_token: "r".into(),
            expires_in: 3600,
            ..Default::default()
        });
        assert_eq!(c.subject, "z@hanzo.ai");
        assert_eq!(c.owner, "hanzo");
        assert_eq!(c.refresh_token, "r");
        assert_eq!(c.token_type, "Bearer");
        assert_ne!(
            c.expiry, 2_000_000_000,
            "expires_in must win over exp claim"
        );
    }

    #[test]
    fn creds_from_token_falls_back_to_exp_claim() {
        let tok = make_jwt(serde_json::json!({"sub": "u-1", "exp": 2_000_000_000i64}));
        let c = creds_from_token(&TokenResp {
            access_token: tok,
            ..Default::default()
        });
        assert_eq!(c.subject, "u-1", "sub used when email absent");
        assert_eq!(c.expiry, 2_000_000_000, "exp claim used when no expires_in");
    }

    #[test]
    fn creds_from_opaque_token_has_no_identity() {
        let c = creds_from_token(&TokenResp {
            access_token: "sk-opaque-not-a-jwt".into(),
            ..Default::default()
        });
        assert_eq!(c.access_token, "sk-opaque-not-a-jwt");
        assert!(c.subject.is_empty());
        assert!(c.owner.is_empty());
    }

    #[test]
    fn short_time_formats() {
        assert_eq!(short_time(0), "unknown");
        assert!(short_time(2_000_000_000).starts_with("2033-"));
    }
}
