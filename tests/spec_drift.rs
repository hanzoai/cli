//! The generated product tree is DERIVED, not maintained. `cargo test` re-runs
//! the derivation over the committed `spec/cloud.json` and fails if the checked-in
//! `generated.rs` is not exactly what comes out — so a hand edit, a half-applied
//! spec refresh, or a generator change landed without regenerating goes red here
//! instead of shipping a command surface no document backs.
//!
//! It runs the SAME binary a maintainer runs (`--check` only decides write vs
//! compare), so the gate can never test a different derivation than the one that
//! writes the file.

#[test]
fn generated_is_exactly_what_the_spec_derives() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_genproduct"))
        .arg("--check")
        .output()
        .expect("run genproduct");
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
///
/// So the assertion moved to the property that actually holds: the spec was
/// generated from hanzoai/cloud at a release tag, it carries that document's
/// sha256, and `.spec-lock` — the file the release writes and the gate re-fetches
/// — says the same two things. Three artifacts, one document, no disagreement.
#[test]
fn the_committed_spec_names_the_release_it_was_generated_from() {
    let root = env!("CARGO_MANIFEST_DIR");
    let spec = std::fs::read_to_string(format!("{root}/spec/cloud.json")).expect("read spec/cloud.json");
    let doc: serde_json::Value = serde_json::from_str(&spec).expect("spec/cloud.json is JSON");
    let prov = doc.pointer("/info/description").and_then(serde_json::Value::as_str).unwrap_or("");

    let lock = std::fs::read_to_string(format!("{root}/.spec-lock")).expect("read .spec-lock");
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
