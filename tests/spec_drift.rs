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
