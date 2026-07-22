//! Runs the agent hook/config fixture suite under `cargo test`.
//!
//! The assertions live in `tests/agent_hooks_config.sh` because they exercise
//! shell-level hook shapes (Codex/Claude/Pi configs and the shared Python
//! wrapper). This harness only wires that script into the normal test run so
//! drift between the four config shapes cannot go unnoticed.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn agent_hook_config_fixtures_pass() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = manifest_dir.join("tests").join("agent_hooks_config.sh");

    let output = Command::new("bash")
        .arg(&script)
        .current_dir(manifest_dir)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", script.display()));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "{} failed with {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        script.display(),
        output.status.code(),
    );
    assert!(
        stdout.contains("agent hook config tests passed"),
        "{} exited 0 without reporting success\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        script.display(),
    );
}
