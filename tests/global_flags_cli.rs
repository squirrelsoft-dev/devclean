//! Integration tests for top-level flag placement: `--force`, `--dry-run`,
//! and `--json` must be accepted both *before* and *after* every applicable
//! subcommand (the global-arg fix). Before the fix, clap rejected
//! `offcut list --force` with "unexpected argument '--force' found" because
//! the flags were not declared `global`.

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
    dir.push(format!(
        "offcut-flags-test-{}-{}.toml",
        std::process::id(),
        n
    ));
    let mut f = std::fs::File::create(&dir).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    dir.to_string_lossy().to_string()
}

fn empty_config() -> String {
    write_config("")
}

/// A flag accepted before the subcommand must also be accepted after it.
/// `offcut list --force` was the reported failure: clap rejected it.
#[test]
fn force_after_subcommand_is_accepted() {
    let cfg = empty_config();
    let out = offcut()
        .args(["--config", &cfg, "list", "--force"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "list --force must be accepted: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dry_run_after_subcommand_is_accepted() {
    let cfg = empty_config();
    let out = offcut()
        .args(["--config", &cfg, "list", "--dry-run"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "list --dry-run must be accepted: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn json_after_subcommand_is_accepted() {
    let cfg = empty_config();
    let out = offcut()
        .args(["--config", &cfg, "list", "--json"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(
        out.status.success(),
        "list --json must be accepted: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"command\":\"list\""), "stdout: {stdout}");
}

/// Both orders parse to the same effective invocation: `--force list` and
/// `list --force` both set force=true in the resolved config echo.
#[test]
fn force_before_and_after_subcommand_are_equivalent() {
    let cfg = empty_config();
    let before = offcut()
        .args(["--force", "--config", &cfg, "config"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let after = offcut()
        .args(["--config", &cfg, "config", "--force"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(before.status.success() && after.status.success());
    let b = String::from_utf8_lossy(&before.stdout);
    let a = String::from_utf8_lossy(&after.stdout);
    assert!(b.contains("default_mode: force"), "before: {b}");
    assert!(a.contains("default_mode: force"), "after: {a}");
}

/// `--dry-run` after `clean` (the destructive subcommand) is accepted and
/// deletes nothing — behavioral, not just parsing.
#[test]
fn dry_run_after_clean_is_accepted_and_non_destructive() {
    let root =
        std::env::temp_dir().join(format!("offcut-flags-clean-dryrun-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let toml = &format!(
        "workspace_roots = [\"{}\"]\nmax_depth = 6\n",
        root.to_str().unwrap()
    );
    let cfg = write_config(toml);
    let out = offcut()
        .args(["--config", &cfg, "clean", "--dry-run"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        out.status.success(),
        "clean --dry-run must be accepted: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--json` after `clean` with `--dry-run` produces a JSON document and
/// deletes nothing.
#[test]
fn json_after_clean_dry_run_is_accepted() {
    let root = std::env::temp_dir().join(format!("offcut-flags-clean-json-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let toml = &format!(
        "workspace_roots = [\"{}\"]\nmax_depth = 6\n",
        root.to_str().unwrap()
    );
    let cfg = write_config(toml);
    let out = offcut()
        .args(["--config", &cfg, "clean", "--json", "--dry-run"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    let _ = std::fs::remove_dir_all(&root);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"command\":\"clean\""), "stdout: {stdout}");
}

/// Every applicable subcommand accepts the three flags after it. This is the
/// regression guard for the global-arg fix: a future refactor that drops
/// `global = true` from one flag fails here.
#[test]
fn each_subcommand_accepts_global_flags_after_it() {
    let cfg = empty_config();
    for sub in ["list", "config", "discovery", "classification"] {
        for flag in ["--force", "--dry-run", "--json"] {
            let out = offcut()
                .args(["--config", &cfg, sub, flag])
                .env("HOME", "/nonexistent-home")
                .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{sub} {flag} must be accepted: stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    // ignore and safelist take a path positional.
    for sub in ["ignore", "safelist"] {
        for flag in ["--force", "--dry-run", "--json"] {
            let out = offcut()
                .args(["--config", &cfg, sub, "somepath", flag])
                .env("HOME", "/nonexistent-home")
                .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{sub} somepath {flag} must be accepted: stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let _ = std::fs::remove_file(&cfg);
}

/// `--config` and `--workspace` are also global: they work after a subcommand.
#[test]
fn config_and_workspace_after_subcommand_are_accepted() {
    let cfg = empty_config();
    let out = offcut()
        .args(["config", "--config", &cfg, "--workspace", "/from/cli"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("- /from/cli"), "stdout: {stdout}");
}
