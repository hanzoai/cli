//! THE DERIVATION GATE — the whole chain, every link, on every `cargo test`.
//!
//! ```text
//!   hanzoai/cloud@<tag> openapi.yaml   .spec-lock       repo, path, ref, sha256
//!        │ genspec
//!   spec/cloud.json                    spec/live.json   `spec_sha256`
//!        │ genproduct
//!   src/commands/product/generated.rs  genproduct --check
//!        ·
//!   spec/live.json                     its own `digest`
//! ```
//!
//! WHAT THIS FILE USED TO BE, and why that was not a gate. It ran `genproduct
//! --check` and asserted that `spec/cloud.json` named a release. Both are
//! comparisons between DERIVED artifacts: `generated.rs` agrees with
//! `spec/cloud.json` for exactly as long as one is derived from the other,
//! whether or not either is still true of any server anywhere. Two stale
//! artifacts agree perfectly. It could not have caught a single defect this repo
//! actually shipped — not the 149 phantom `/v1/cloud/*` commands, not the 46
//! products a stale capture left unreachable, not the four `load-balancers`
//! commands deleted upstream nine minutes after the spec that shipped them.
//!
//! What decides whether a command is real is the SERVER. So `driftgate --refresh`
//! asks it — with a CONTROL for every probe, because a relay door answers
//! identically for a real path and an invented one — and writes down the raw
//! answers as `spec/live.json`. This file then RULES on them, hermetically: no
//! network, no clock, no host. A gate that needs the network in CI gets switched
//! off, and a switched-off gate is worse than none.

use std::process::Command;

fn root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

/// The generated product tree is DERIVED, not maintained. This re-runs the
/// derivation over the committed `spec/cloud.json` and fails if the checked-in
/// `generated.rs` is not exactly what comes out — so a hand edit, a half-applied
/// spec refresh, or a generator change landed without regenerating goes red here
/// instead of shipping a command surface no document backs.
///
/// It runs the SAME binary a maintainer runs (`--check` only decides write vs
/// compare), so the gate can never test a different derivation than the one that
/// writes the file.
#[test]
fn generated_is_exactly_what_the_spec_derives() {
    let out = Command::new(env!("CARGO_BIN_EXE_genproduct")).arg("--check").output().expect("run genproduct");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// …and the spec names a RELEASE, by ref and by digest, and `.spec-lock` agrees.
///
/// This used to assert the spec named the wire (`https://api.hanzo.ai/...`), for
/// a reason that was half right: a capture taken from some local file is a
/// photograph of a moment, and two such photographs shipped as defects — 149
/// phantom `/v1/cloud/*` commands survived one, and 41 platform commands printed
/// their HTTP route where their description belonged because of another.
///
/// But the wire is a photograph too, and a worse one: it names a HOST, and a
/// host cannot say which deploy it was. A capture built from the wire keeps
/// claiming the wire forever while the router moves underneath it, so
/// "provenance names the wire" was satisfied by every stale spec ever committed
/// — it caught neither defect it was written for. What catches them is
/// `genspec --check` re-running the derivation on every push, and that needs a
/// document a ref can NAME.
#[test]
fn the_committed_spec_names_the_release_it_was_generated_from() {
    let spec = std::fs::read_to_string(format!("{}/spec/cloud.json", root())).expect("read spec/cloud.json");
    let doc: serde_json::Value = serde_json::from_str(&spec).expect("spec/cloud.json is JSON");
    let prov = doc.pointer("/info/description").and_then(serde_json::Value::as_str).unwrap_or("");

    let lock = std::fs::read_to_string(format!("{}/.spec-lock", root())).expect("read .spec-lock");
    let field = |k: &str| {
        lock.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!(".spec-lock has no {k}="))
            .to_string()
    };
    let (r#ref, sha) = (field("ref"), field("sha256"));

    assert!(
        r#ref.starts_with('v') && r#ref.split('.').count() == 3,
        ".spec-lock names {ref:?}, which is not a hanzoai/cloud release tag. A capture must name a \
         RELEASE: main is a document nobody has deployed, and a host is not a version."
    );
    assert!(
        prov.contains(&format!("hanzoai/cloud@{ref}")) && prov.contains(&format!("sha256:{sha}")),
        "spec/cloud.json and .spec-lock disagree about which document this capture is.\n  \
         spec says: {prov}\n  lock says: hanzoai/cloud@{ref} sha256:{sha}\n\
         Re-run `make spec`, then commit spec/cloud.json, generated.rs and .spec-lock together."
    );
}

/// THE DRIFT GATE ITSELF, run on every `cargo test` because it now needs nothing
/// but a checkout. It asks BOTH questions the two above cannot:
///
///   * is every coordinate a command addresses still SERVED? (evidence from the
///     host, with a control probe, so a `403` wall answering for everything
///     cannot read as "present" and a `404` means what it says)
///   * is every served product REACHABLE, asked of the built binary, unless
///     `src/curation.rs` excuses it with a reason?
///
/// …and it refuses a hand-edited artifact anywhere in the chain: `spec/live.json`
/// carries the digest of the `spec/cloud.json` it is evidence about and a digest
/// of its own payload, so an edit to either stops matching.
///
/// It execs the same binary `make verify` runs, for the same reason
/// `generated_is_exactly_what_the_spec_derives` execs `genproduct`: a gate that
/// re-implements the rule it is gating tests its own copy of it.
#[test]
fn the_live_server_contradicts_this_command_tree_in_neither_direction() {
    let out = Command::new(env!("CARGO_BIN_EXE_driftgate"))
        .args(["--hanzo", env!("CARGO_BIN_EXE_hanzo")])
        .output()
        .expect("run driftgate");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
