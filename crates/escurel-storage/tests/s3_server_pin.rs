//! The live workflow pre-pulls the S3 test server's image so a slow or dead
//! registry shows up as its own step, not as a per-test panic. That pull names
//! the image in YAML; this test keeps it in step with the Rust constant.
//! (MinIO's image vanished from two registries once; the next change of server
//! should be one edit that the CI cannot get half-done.)

#![cfg(feature = "test-s3-server")]

use escurel_storage::testing::{IMAGE, TAG};

#[test]
fn the_live_workflow_pre_pulls_the_image_the_tests_start() {
    let workflow =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/live.yml");
    let yaml = std::fs::read_to_string(&workflow)
        .unwrap_or_else(|err| panic!("read {}: {err}", workflow.display()));
    let pull = format!("docker pull {IMAGE}:{TAG}");
    assert!(
        yaml.contains(&pull),
        "live.yml must pre-pull the exact image the tests start (`{pull}`); \
         `escurel_storage::testing` and the workflow drifted apart"
    );
}
