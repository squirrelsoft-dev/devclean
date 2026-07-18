//! Integration tests for the `devclean safelist` debug subcommand, which is
//! the observable hook for the safe-to-delete catalog (issue #4). The catalog
//! module itself is unit-tested in `src/safelist.rs`; these tests exercise the
//! full CLI path: loading the default config, building the safe-to-delete set,
//! and reporting per-path results.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);
static ISOLATED_HOME: OnceLock<PathBuf> = OnceLock::new();

/// An empty directory standing in for the developer's home, shared by every
/// child process this file spawns. Without it `config::default_config_path`
/// resolves against the real `HOME`/`XDG_CONFIG_HOME`, so a developer who
/// happens to have a `devclean/config.toml` would silently change what these
/// tests assert — and a malformed one would fail them for unrelated reasons.
fn isolated_home() -> &'static Path {
    ISOLATED_HOME.get_or_init(|| {
        let mut d = std::env::temp_dir();
        d.push(format!("devclean-safelist-cli-home-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

fn devclean() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devclean"));
    let home = isolated_home();
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd
}

fn unique_dir(label: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    d.push(format!(
        "devclean-safelist-cli-{}-{}-{}",
        label,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run `devclean safelist <path>` inside `cwd`, returning the trimmed stdout.
/// The child runs against [`isolated_home`], so the platform default config
/// path resolves inside an empty directory rather than the developer's real
/// home.
fn run_in(cwd: &Path, path: &str) -> (bool, String) {
    let result = devclean()
        .current_dir(cwd)
        .arg("safelist")
        .arg(path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    (result.status.success(), format!("{stdout}\n{stderr}"))
}

/// Same as [`run_in`], but with `extra_args` (typically `--config` plus a path)
/// threaded before the subcommand — clap requires top-level flags to precede
/// the subcommand, so tests that exercise `--config` must inject them before
/// `arg("safelist")`.
fn run_in_with(cwd: &Path, extra_args: &[&str], path: &str) -> (bool, String) {
    let mut cmd = devclean();
    cmd.current_dir(cwd);
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg("safelist").arg(path);
    let result = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    (result.status.success(), format!("{stdout}\n{stderr}"))
}

#[test]
fn safelist_subcommand_reports_safe_and_not_safe() {
    // Built-in pattern: node_modules is safe at any depth.
    let root = unique_dir("basic");

    let (ok, out) = run_in(&root, "node_modules");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "node_modules: safe");

    let (ok, out) = run_in(&root, "src/node_modules");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "src/node_modules: safe");

    // Non-listed file: not safe.
    let (ok, out) = run_in(&root, "README.md");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "README.md: not-safe");
}

#[test]
fn safelist_subcommand_with_default_config_falls_back_on_missing_file() {
    // The isolated home has no `devclean/config.toml`, so this genuinely
    // exercises the missing-default-file fallback: the built-in set, whole.
    let root = unique_dir("default");

    let (ok, out) = run_in(&root, "target");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "target: safe");
}

#[test]
fn safelist_subcommand_with_explicit_config_uses_additions() {
    // A user-supplied pattern extends the built-in set. The subcommand loads
    // the user's explicit `--config` path and adds its entries on top.
    let root = unique_dir("custom");
    let mut cfg_path = root.clone();
    cfg_path.push("devclean.toml");
    std::fs::write(&cfg_path, "safe_delete = [\"**/my_artifacts\"]\n").unwrap();

    let cfg_str = cfg_path.to_str().unwrap();

    let (ok, out) = run_in_with(&root, &["--config", cfg_str], "my_artifacts");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "my_artifacts: safe");

    // Built-in still matches alongside the addition.
    let (ok, out) = run_in_with(&root, &["--config", cfg_str], "target");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "target: safe");

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn safelist_subcommand_with_malformed_pattern_reports_an_error() {
    // A bad glob in the user's `safe_delete` is a config mistake, so it must
    // surface as `devclean: <error>` + a non-zero exit like every other config
    // problem — never as a panic with a backtrace.
    let root = unique_dir("malformed");
    let cfg_path = root.join("devclean.toml");
    std::fs::write(&cfg_path, "safe_delete = [\"[z-a]\"]\n").unwrap();

    let (ok, out) = run_in_with(
        &root,
        &["--config", cfg_path.to_str().unwrap()],
        "node_modules",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("devclean: "), "{out}");
    assert!(!out.contains("panicked"), "{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn safelist_subcommand_reports_a_directory_only_pattern_by_path_kind() {
    // The subcommand must stat the path rather than assuming it is a
    // directory: a user's `build/`-style pattern should not report a regular
    // file of the same name as safe to delete.
    let root = unique_dir("dir-only");
    let cfg_path = root.join("devclean.toml");
    std::fs::write(&cfg_path, "safe_delete = [\"**/artifacts/\"]\n").unwrap();
    let cfg_str = cfg_path.to_str().unwrap();

    std::fs::write(root.join("artifacts"), b"not a directory\n").unwrap();
    let (ok, out) = run_in_with(&root, &["--config", cfg_str], "artifacts");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "artifacts: not-safe");

    std::fs::remove_file(root.join("artifacts")).unwrap();
    std::fs::create_dir(root.join("artifacts")).unwrap();
    let (ok, out) = run_in_with(&root, &["--config", cfg_str], "artifacts");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "artifacts: safe");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn safelist_subcommand_rejects_paths_outside_the_project_root() {
    // Reporting "safe" for an unanswerable path would be a false positive in
    // exactly the place one is most dangerous: the user would read it as
    // "safe to delete".
    let root = unique_dir("outside");

    let (ok, out) = run_in(&root, "/etc/hosts");
    assert!(!ok, "{out}");
    assert!(out.contains("outside the project root"), "{out}");

    let (ok, out) = run_in(&root, "../escape.log");
    assert!(!ok, "{out}");
    assert!(out.contains("escapes the project root"), "{out}");
}

#[test]
fn safelist_subcommand_with_missing_explicit_config_is_an_error() {
    // A path the user typed explicitly must exist — the config module itself
    // is allowed to fall back to defaults at the default location, but a
    // caller-supplied path is not.
    let root = unique_dir("missing-config");
    let cfg_arg = root.join("nope.toml");

    let (ok, out) = run_in_with(
        &root,
        &["--config", cfg_arg.to_str().unwrap()],
        "node_modules",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("not found"), "{out}");

    let _ = std::fs::remove_dir_all(&root);
}
