use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn devclean() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devclean"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_config(contents: &str) -> String {
    let mut dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    dir.push(format!(
        "devclean-cli-test-{}-{}.toml",
        std::process::id(),
        n
    ));
    let mut f = std::fs::File::create(&dir).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    dir.to_string_lossy().to_string()
}

const BANNER: &str = "devclean - development environment cleanup CLI (scaffold)";

#[test]
fn version_flag_prints_crate_version() {
    let out = devclean().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("devclean {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_args_prints_placeholder_banner() {
    let out = devclean().output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), BANNER);
}

#[test]
fn help_flag_lists_version_and_help_options() {
    let out = devclean().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(BANNER));
    assert!(stdout.contains("--version"));
    assert!(stdout.contains("--help"));
}

#[test]
fn unknown_argument_is_rejected() {
    let out = devclean().arg("--bogus").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
}

#[test]
fn list_prints_defaults_when_no_config() {
    let cfg = write_config("");
    let out = devclean()
        .args(["--config", &cfg, "list"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("max_depth: 4"), "stdout: {stdout}");
    assert!(
        stdout.contains("default_mode: interactive"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("workspace_roots:"), "stdout: {stdout}");
    assert!(stdout.contains("(none)"), "stdout: {stdout}");
}

#[test]
fn explicit_missing_config_is_an_error() {
    let mut missing = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    missing.push(format!(
        "devclean-cli-absent-{}-{}.toml",
        std::process::id(),
        n
    ));
    let out = devclean()
        .args(["--config", &missing.to_string_lossy(), "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config file not found"), "stderr: {stderr}");
}

#[test]
fn force_and_dry_run_used_together_run_clean() {
    // A config whose single workspace root exists (and is empty), so the
    // combined flags exercise the clean flow without finding anything.
    let each_dir = std::env::temp_dir().join("devclean-cli-test-each");
    std::fs::create_dir_all(&each_dir).unwrap();
    let toml =
        &format!("workspace_roots = [\"{}\"]\nmax_depth = 6\ndefault_mode = \"interactive\"\n", each_dir.to_str().unwrap());
    let cfg = write_config(toml);
    let out = devclean()
        .args(["--force", "--dry-run", "--config", &cfg, "clean"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        panic!("force and dry-run failed; stdout: {stdout:?}; stderr: {stderr:?}");
    }
}

#[test]
fn list_reflects_config_file_and_cli_overrides() {
    let toml =
        "workspace_roots = [\"/from/config\"]\nmax_depth = 6\ndefault_mode = \"interactive\"\n";
    let cfg = write_config(toml);
    let out = devclean()
        .args([
            "--config",
            &cfg,
            "--workspace",
            "/from/cli",
            "--force",
            "list",
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("max_depth: 6"), "stdout: {stdout}");
    // CLI --force overrides config interactive.
    assert!(stdout.contains("default_mode: force"), "stdout: {stdout}");
    // Both config and CLI workspace roots appear (append semantics).
    assert!(stdout.contains("- /from/config"), "stdout: {stdout}");
    assert!(stdout.contains("- /from/cli"), "stdout: {stdout}");
    assert!(stdout.contains("force=true"), "stdout: {stdout}");
}
