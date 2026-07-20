use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn offcut() -> Command {
    Command::new(env!("CARGO_BIN_EXE_offcut"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_config(contents: &str) -> String {
    let mut dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    dir.push(format!("offcut-cli-test-{}-{}.toml", std::process::id(), n));
    let mut f = std::fs::File::create(&dir).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    dir.to_string_lossy().to_string()
}

/// Each subcommand is optional — the default run executes the clean flow
/// without a subcommand. This test confirms no-args triggers the clean flow
/// (which, with no config, prints "clean: no projects found" and exits 0).
/// `HOME`/`XDG_CONFIG_HOME` are overridden so the run never picks up the
/// developer's real config (whose workspace roots would yield projects).
#[test]
fn no_args_runs_the_default_clean_flow() {
    let out = offcut()
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // With no workspace roots configured and no --config, discovery yields
    // nothing, so the clean flow prints "no projects found" and exits 0.
    assert!(
        stdout.contains("no projects found"),
        "no-args should run the clean flow: {stdout:?}"
    );
}

#[test]
fn version_flag_prints_crate_version() {
    let out = offcut().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("offcut {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_flag_lists_version_and_help_options() {
    let out = offcut().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("development environment cleanup CLI"));
    assert!(stdout.contains("--version"));
    assert!(stdout.contains("--help"));
    assert!(
        !stdout.contains("devclean"),
        "help output should not mention the old binary name: {stdout}"
    );
}

#[test]
fn unknown_argument_is_rejected() {
    let out = offcut().arg("--bogus").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
}

#[test]
fn list_prints_defaults_when_no_config() {
    // The original `list` subcommand printed the resolved config. Now `list`
    // lists projects + statuses; the old behavior lives under `offcut
    // config`. This test verifies the preserved behavior.
    let cfg = write_config("");
    let out = offcut()
        .args(["--config", &cfg, "config"])
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
        "offcut-cli-absent-{}-{}.toml",
        std::process::id(),
        n
    ));
    let out = offcut()
        .args(["--config", &missing.to_string_lossy(), "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config file not found"), "stderr: {stderr}");
}

#[test]
fn force_and_dry_run_combine_as_force_preview() {
    // `--force --dry-run` is a valid combination (the #8 invariant): it
    // previews the force run. This test only confirms clap accepts both
    // flags together; the preview semantics are covered by
    // `clean_force_dry_run_auto_approves_each_item` in tests/clean_cli.rs.
    let each_dir = std::env::temp_dir().join("offcut-cli-test-each");
    std::fs::create_dir_all(&each_dir).unwrap();
    let toml = &format!(
        "workspace_roots = [\"{}\"]\nmax_depth = 6\ndefault_mode = \"interactive\"\n",
        each_dir.to_str().unwrap()
    );
    let cfg = write_config(toml);
    let out = offcut()
        .args(["--force", "--dry-run", "--config", &cfg, "clean"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "the flags must combine: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn list_reflects_config_file_and_cli_overrides() {
    // The original `list` subcommand printed the resolved config. Now `list`
    // lists projects + statuses; the old behavior lives under `offcut
    // config`. This test verifies the preserved behavior.
    let toml =
        "workspace_roots = [\"/from/config\"]\nmax_depth = 6\ndefault_mode = \"interactive\"\n";
    let cfg = write_config(toml);
    let out = offcut()
        .args([
            "--config",
            &cfg,
            "--workspace",
            "/from/cli",
            "--force",
            "config",
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
