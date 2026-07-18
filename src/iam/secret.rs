//! The ONE stdin-secret law. A secret — a KMS secret value, a provider API key,
//! an identity JWT — arrives on STDIN or a hidden interactive prompt, NEVER on
//! argv. That is the whole invariant: no `ps`, no `~/.zsh_history`, no CI log can
//! ever hold it, because there is no argument to carry it by.
//!
//! Two concerns live here, decomplected:
//! - WHERE a secret may come from ([`secret_source`]) — the argv-refusal
//!   decision, pure and identical for every secret. `iam::onboarding` (keys +
//!   identity tokens) resolves through it.
//! - HOW to read one from a stream — [`read_secret`] for a RAW secret value
//!   (KMS: strip one trailing newline, keep every other byte), [`read_trimmed`]
//!   for a KEY (no meaningful surrounding whitespace, so it trims). One law,
//!   two encodings that differ only by what the secret's own bytes mean.
//!
//! The `hanzo <product>` generated tree honors the same law structurally: a
//! `format: password` field gets no flag, and its value is read here at dispatch.

use anyhow::{bail, Context, Result};
use std::io::Read;

/// Where a login secret may come from, decided from the `--token` flag and
/// whether stdin is a terminal. PURE — the actual read is the caller's — so the
/// "never argv" invariant is unit-testable and shared by every secret input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    /// Read the whole of stdin (`--token -`, or a pipe with no flag).
    Stdin,
    /// Prompt interactively with a hidden input (a terminal, no `--token`).
    Prompt,
    /// A literal was passed in argv — REFUSED: it would land in `ps`/history.
    ArgvRefused,
}

/// The argv-refusal decision. `Some("-")` (or a pipe with no flag) reads stdin;
/// any other literal is refused; a bare terminal prompts.
pub fn secret_source(token: Option<&str>, stdin_tty: bool) -> SecretSource {
    match token {
        Some("-") => SecretSource::Stdin,
        Some(_) => SecretSource::ArgvRefused,
        None if !stdin_tty => SecretSource::Stdin, // piped input
        None => SecretSource::Prompt,
    }
}

/// Read a RAW secret value from `r`.
///
/// Exactly ONE trailing newline is stripped (`\n` or `\r\n`), because
/// `printf %s "$V" |` and `echo "$V" |` are both common and storing the shell's
/// line terminator would silently corrupt the value for every consumer. Nothing
/// else is touched — leading and interior bytes (spaces, a PEM's inner newlines)
/// are the value. Empty is refused rather than stored.
///
/// This is the reader the `kms secrets create` value uses.
pub fn read_secret<R: Read>(mut r: R) -> Result<String> {
    let mut v = String::new();
    r.read_to_string(&mut v).context("reading the secret value from stdin")?;
    if let Some(s) = v.strip_suffix('\n') {
        v = s.strip_suffix('\r').unwrap_or(s).to_string();
    }
    if v.is_empty() {
        bail!("no value on stdin — pipe the secret in, e.g. `printf %s \"$V\" | hanzo kms secrets create --name NAME --env prod`");
    }
    Ok(v)
}

/// Read a KEY from `r`, fully trimmed; error if empty. A provider/identity key
/// carries no meaningful surrounding whitespace (unlike a raw secret value), so
/// it trims. The stdin path for `iam::onboarding`.
pub fn read_trimmed<R: Read>(mut r: R) -> Result<String> {
    let mut s = String::new();
    r.read_to_string(&mut s).context("reading key from stdin")?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("no key provided on stdin");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE invariant: a secret never comes from argv. A literal is refused; a
    /// `-` (or a pipe) reads stdin; a bare terminal prompts.
    #[test]
    fn secret_source_never_takes_a_secret_from_argv() {
        assert_eq!(secret_source(Some("-"), true), SecretSource::Stdin);
        assert_eq!(secret_source(Some("-"), false), SecretSource::Stdin);
        // A literal on the command line is ALWAYS refused, TTY or not.
        assert_eq!(secret_source(Some("sk-ant-literal"), true), SecretSource::ArgvRefused);
        assert_eq!(secret_source(Some("sk-ant-literal"), false), SecretSource::ArgvRefused);
        // No flag: a pipe feeds stdin; an interactive terminal prompts.
        assert_eq!(secret_source(None, false), SecretSource::Stdin);
        assert_eq!(secret_source(None, true), SecretSource::Prompt);
    }

    /// A raw value keeps its bytes: exactly one trailing newline is stripped, and
    /// leading/interior bytes and a genuine trailing blank line survive.
    #[test]
    fn read_secret_strips_exactly_one_trailing_newline() {
        assert_eq!(read_secret(&b"hunter2\n"[..]).unwrap(), "hunter2");
        assert_eq!(read_secret(&b"hunter2\r\n"[..]).unwrap(), "hunter2");
        assert_eq!(read_secret(&b"hunter2"[..]).unwrap(), "hunter2");
        // Only ONE: a value that really ends in a blank line keeps it.
        assert_eq!(read_secret(&b"hunter2\n\n"[..]).unwrap(), "hunter2\n");
        // Interior and leading bytes are the value, never trimmed.
        assert_eq!(read_secret(&b"  a b\nc  \n"[..]).unwrap(), "  a b\nc  ");
    }

    #[test]
    fn read_secret_refuses_empty() {
        assert!(read_secret(&b""[..]).is_err());
        assert!(read_secret(&b"\n"[..]).is_err());
    }

    /// A key trims surrounding whitespace (it has none of its own) and refuses
    /// empty.
    #[test]
    fn read_trimmed_trims_and_rejects_empty() {
        assert_eq!(read_trimmed(std::io::Cursor::new("  sk-ant-xyz\n")).unwrap(), "sk-ant-xyz");
        assert_eq!(read_trimmed(std::io::Cursor::new("hk-abc")).unwrap(), "hk-abc");
        assert!(read_trimmed(std::io::Cursor::new("   \n ")).is_err(), "whitespace-only is empty");
        assert!(read_trimmed(std::io::Cursor::new("")).is_err());
    }
}
