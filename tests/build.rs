//! `hanzo build` against a real BuildKit. The shipped binary boots a hanzo-vm
//! microVM, starts buildkitd in it, builds a Dockerfile whose RUN step executes
//! in the guest, and writes the image back to this machine as an OCI archive —
//! so the test fails whenever the command cannot reach BuildKit.
//!
//! It needs a hypervisor (Virtualization.framework, or /dev/kvm) and the
//! network, so `cargo test` lists it as ignored. Run it with
//! `cargo test --test build -- --ignored`.

use assert_cmd::Command;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[test]
#[ignore = "boots a hanzo-vm microVM: cargo test --test build -- --ignored"]
fn a_dockerfile_builds_in_the_vm_and_comes_back_as_an_oci_archive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM busybox:1.36\nCOPY note /note\nRUN cat /note > /built && uname -m >> /built\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("note"), "from the host\n").unwrap();
    let archive = dir.path().join("image.tar");

    Command::cargo_bin("hanzo")
        .unwrap()
        .args(["build", "--cpus", "2", "--memory", "2048"])
        .arg(dir.path())
        .args(["-t", "localhost/hanzo-build-test:e2e", "-o"])
        .arg(&archive)
        .assert()
        .success();

    // The guest runs the host's architecture, so the RUN step reports it.
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    assert_eq!(
        file_in_image(&archive, "built"),
        format!("from the host\n{arch}\n")
    );
}

/// Read `name` out of the image an OCI archive holds, top layer first.
fn file_in_image(archive: &Path, name: &str) -> String {
    let mut blobs = HashMap::new();
    let mut tar = tar::Archive::new(std::fs::File::open(archive).expect("the archive was written"));
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        blobs.insert(path, bytes);
    }
    let blob = |digest: &Value| &blobs[&format!("blobs/sha256/{}", &digest.as_str().unwrap()[7..])];
    let mut doc: Value = serde_json::from_slice(&blobs["index.json"]).unwrap();
    // Descend through any index to the linux image manifest.
    while let Some(manifests) = doc.get("manifests").and_then(Value::as_array) {
        let image = manifests
            .iter()
            .find(|m| m["platform"]["os"].as_str().is_none_or(|os| os == "linux"))
            .expect("an image manifest");
        doc = serde_json::from_slice(blob(&image["digest"])).unwrap();
    }
    for layer in doc["layers"].as_array().unwrap().iter().rev() {
        let gz = blob(&layer["digest"]);
        let mut files = tar::Archive::new(flate2::read::GzDecoder::new(&gz[..]));
        for entry in files.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().to_str() == Some(name) {
                let mut text = String::new();
                entry.read_to_string(&mut text).unwrap();
                return text;
            }
        }
    }
    panic!("{name} is in no layer of the image");
}
