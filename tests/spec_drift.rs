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

/// …and the spec itself came from the WIRE.
///
/// `genspec` records its own provenance in `info.description`. A spec built with
/// `--registry <local file>` is a photograph of a moment: it cannot refute what
/// the live table would refute, and it cannot carry prose the live table has
/// since gained. Both halves of that have already shipped as defects — a captured
/// table let 149 phantom `/v1/cloud/*` commands survive, and a captured table is
/// why 41 platform commands printed their HTTP route where their description
/// belonged. One assertion closes both: the committed spec must name the wire.
#[test]
fn the_committed_spec_was_built_from_the_live_route_table() {
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/spec/cloud.json"))
        .expect("read spec/cloud.json");
    let doc: serde_json::Value = serde_json::from_str(&spec).expect("spec/cloud.json is JSON");
    let prov = doc.pointer("/info/description").and_then(serde_json::Value::as_str).unwrap_or("");
    assert!(
        prov.contains("https://api.hanzo.ai/v1/openapi.json"),
        "spec/cloud.json was not built from the live route table — its provenance reads:\n  {prov}\n\
         Re-run `cargo run --features genspec --bin genspec --  --openapi <hanzoai/openapi checkout>` \
         with NO --registry (the default IS the wire), then `cargo run --bin genproduct`."
    );
}
