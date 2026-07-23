//! Integration tests for `--json` machine-readable output mode.
//!
//! Each result-producing command emits exactly one valid JSON document on
//! stdout under `--json`: no ANSI, no progress, no prompts, no extra text.
//! Diagnostics stay on stderr; exit codes remain meaningful (0 success,
//! 1 error, 2 approval-required). `--json` never prompts and never broadens
//! deletion authority.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn offcut() -> Command {
    Command::new(env!("CARGO_BIN_EXE_offcut"))
}

fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!("offcut-json-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    out
}

fn write_config(home: &Path, roots: &[&str], depth: usize) -> PathBuf {
    let p = home.join("config.toml");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    let quoted = roots
        .iter()
        .map(|r| format!("{:?}", r))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(f, "workspace_roots = [{quoted}]").unwrap();
    writeln!(f, "max_depth = {}", depth).unwrap();
    writeln!(
        f,
        "project_markers = [\".git\", \"package.json\", \"Cargo.toml\", \"go.mod\"]"
    )
    .unwrap();
    p
}

fn empty_config() -> String {
    let mut dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    dir.push(format!(
        "offcut-json-empty-{}-{}.toml",
        std::process::id(),
        n
    ));
    let mut f = std::fs::File::create(&dir).unwrap();
    f.write_all(b"").unwrap();
    dir.to_string_lossy().to_string()
}

fn git_in(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git -C {} {:?} failed",
        root.display(),
        args
    );
}

fn init_repo_with_commit(root: &Path) {
    git_in(root, &["init"]);
    git_in(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git_in(root, &["config", "user.email", "test@test.dev"]);
    git_in(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("initial.txt"), "initial").unwrap();
    git_in(root, &["add", "initial.txt"]);
    git_in(root, &["commit", "-m", "initial"]);
}

fn add_pushed_remote(root: &Path) -> PathBuf {
    let bare = root_for(&format!(
        "{}-bare",
        root.file_stem().unwrap().to_string_lossy()
    ));
    git_in(&bare, &["init", "--bare"]);
    git_in(root, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git_in(root, &["config", "branch.main.remote", "origin"]);
    git_in(root, &["config", "branch.main.merge", "refs/heads/main"]);
    git_in(root, &["push", "origin", "main"]);
    bare
}

/// Run with `--json` and return (exit_status, parsed_json, stdout_raw, stderr).
fn run_json<I, S>(args: I) -> (std::process::ExitStatus, serde_json::Value, String, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = offcut()
        .args(args)
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be one JSON document: {e}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"));
    (out.status, parsed, stdout, stderr)
}

fn assert_clean_json(stdout: &str) {
    assert!(
        !stdout.contains('\u{1b}'),
        "JSON stdout must carry no ANSI escapes: {stdout:?}"
    );
    assert!(
        !stdout.contains('\r'),
        "JSON stdout must carry no live-progress CR: {stdout:?}"
    );
    // Exactly one JSON document: the whole stdout parses and there is only a
    // trailing newline after the document.
    let trimmed = stdout.strip_suffix('\n').unwrap_or(stdout);
    let _v: serde_json::Value =
        serde_json::from_str(trimmed).expect("exactly one JSON document on stdout");
    assert!(
        !trimmed[..].trim_end().ends_with('}') || trimmed.trim_end() == trimmed.trim_start(),
        "no trailing text after the document"
    );
}

/// `list --json` with no projects: a valid document, ok=true, empty array.
#[test]
fn json_list_empty_is_valid_document() {
    let cfg = empty_config();
    let (status, v, stdout, _stderr) = run_json(["--config", &cfg, "list", "--json"]);
    let _ = std::fs::remove_file(&cfg);
    assert!(status.success());
    assert_eq!(v["version"], 1);
    assert_eq!(v["command"], "list");
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["projects"].as_array().unwrap().len(), 0);
    assert_eq!(v["result"]["cleanable_count"], 0);
    assert_clean_json(&stdout);
}

/// `list --json` with a cleanable project carries status, rank, and
/// reclaimable_bytes.
#[test]
fn json_list_with_cleanable_project() {
    let root = root_for("list-cleanable");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home-list-cleanable");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) =
        run_json(["--config", config.to_str().unwrap(), "list", "--json"]);
    assert!(status.success());
    let projects = v["result"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["status"], "cleanable");
    assert_eq!(projects[0]["rank"], 5);
    assert!(
        projects[0]["reclaimable_bytes"].as_u64().unwrap() >= 4096,
        "reclaimable_bytes present for cleanable: {}",
        projects[0]
    );
    assert_eq!(v["result"]["cleanable_count"], 1);
    assert!(v["result"]["total_reclaimable_bytes"].as_u64().unwrap() >= 4096);
    assert_clean_json(&stdout);
}

/// `config --json` echoes the resolved config.
#[test]
fn json_config_echoes_resolved_config() {
    let cfg = empty_config();
    let (status, v, stdout, _stderr) = run_json(["--config", &cfg, "config", "--json", "--force"]);
    let _ = std::fs::remove_file(&cfg);
    assert!(status.success());
    assert_eq!(v["command"], "config");
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["default_mode"], "force");
    assert_eq!(v["result"]["max_depth"], 4);
    assert_eq!(v["result"]["force"], true);
    assert!(v["result"]["config_file"].is_string());
    assert_clean_json(&stdout);
}

/// `discovery --json` lists discovered projects with markers.
#[test]
fn json_discovery_lists_projects() {
    let root = root_for("discovery");
    init_repo_with_commit(&root);
    let home = root_for("home-discovery");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) =
        run_json(["--config", config.to_str().unwrap(), "discovery", "--json"]);
    assert!(status.success());
    let projects = v["result"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert!(projects[0]["path"].is_string());
    assert!(projects[0]["marker"].is_string());
    assert_clean_json(&stdout);
}

/// `classification --json` carries status and rank per project.
#[test]
fn json_classification_carries_status_and_rank() {
    let root = root_for("classification");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "junk").unwrap();
    let home = root_for("home-classification");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) = run_json([
        "--config",
        config.to_str().unwrap(),
        "classification",
        "--json",
    ]);
    assert!(status.success());
    let projects = v["result"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["status"], "cleanable");
    assert_eq!(projects[0]["rank"], 5);
    assert_clean_json(&stdout);
}

/// `ignore --json` reports ignored/not-ignored.
#[test]
fn json_ignore_reports_match() {
    let root = root_for("ignore-cwd");
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    std::fs::write(root.join("important.dat"), "keep").unwrap();
    let out = offcut()
        .args(["ignore", "important.dat", "--json"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .current_dir(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["command"], "ignore");
    assert_eq!(v["result"]["ignored"], true);
    assert_clean_json(&stdout);
}

/// `safelist --json` reports safe/not-safe.
#[test]
fn json_safelist_reports_match() {
    let out = offcut()
        .args(["safelist", "target", "--json"])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["command"], "safelist");
    assert_eq!(v["result"]["safe"], true);
    assert_clean_json(&stdout);
}

/// `init --json` reports the created config file path.
#[test]
fn json_init_reports_created_file() {
    let target = std::env::temp_dir().join(format!(
        "offcut-json-init-{}-{}.toml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&target);
    let (status, v, stdout, _stderr) = run_json([
        "--config",
        target.to_str().unwrap(),
        "init",
        "/tmp/json-init-workspace",
        "--json",
    ]);
    let _ = std::fs::remove_file(&target);
    assert!(status.success());
    assert_eq!(v["command"], "init");
    assert_eq!(v["result"]["created"], true);
    assert!(v["result"]["config_file"].is_string());
    assert_clean_json(&stdout);
}

/// `clean --json --dry-run` previews without deleting: ok=true, exit 0, fate
/// `would-delete` for safe items, nothing deleted.
#[test]
fn json_clean_dry_run_previews_without_deleting() {
    let root = root_for("clean-dryrun");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-clean-dryrun");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) = run_json([
        "--config",
        config.to_str().unwrap(),
        "clean",
        "--json",
        "--dry-run",
    ]);
    assert!(status.success());
    assert_eq!(v["command"], "clean");
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["approval_required"], false);
    let projects = v["result"]["projects"].as_array().unwrap();
    let cleanable = projects
        .iter()
        .find(|p| p["status"] == "cleanable")
        .expect("a cleanable project");
    let fates = cleanable["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["path"].as_str().unwrap(), i["fate"].as_str().unwrap()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(fates.get("target"), Some(&"would-delete"));
    assert_eq!(fates.get("ambiguous.tmp"), Some(&"would-prompt"));
    assert_eq!(cleanable["deleted_count"], 0);
    // Nothing deleted.
    assert!(root.join("target/bin").is_file());
    assert!(root.join("ambiguous.tmp").is_file());
    assert_clean_json(&stdout);
}

/// `clean --json --force` deletes safe + surfaced items, reports
/// `deleted`, ok=true, exit 0.
#[test]
fn json_clean_force_deletes_and_reports() {
    let root = root_for("clean-force");
    init_repo_with_commit(&root);
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-clean-force");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) = run_json([
        "--config",
        config.to_str().unwrap(),
        "clean",
        "--json",
        "--force",
    ]);
    assert!(status.success());
    assert_eq!(v["result"]["approval_required"], false);
    let cleanable = v["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["status"] == "cleanable")
        .unwrap();
    assert_eq!(cleanable["approved"], true);
    assert!(cleanable["deleted_count"].as_u64().unwrap() >= 2);
    let fates = cleanable["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["path"].as_str().unwrap(), i["fate"].as_str().unwrap()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(fates.get("target"), Some(&"deleted"));
    assert_eq!(fates.get("ambiguous.tmp"), Some(&"deleted"));
    assert_eq!(fates.get("important.dat"), Some(&"kept"));
    assert!(!root.join("target").exists());
    assert!(!root.join("ambiguous.tmp").exists());
    assert!(root.join("important.dat").is_file());
    assert_clean_json(&stdout);
}

/// `clean --json` with neither --force nor --dry-run cannot prompt: it
/// returns approval_required=true, deletes nothing, and exits 3.
/// Exit 3 (not 2) so it is distinct from clap's usage-error exit code.
#[test]
fn json_clean_without_force_or_dry_run_is_approval_required() {
    let root = root_for("clean-approval");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-clean-approval");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, stderr) =
        run_json(["--config", config.to_str().unwrap(), "clean", "--json"]);
    // Exit code 3 = approval required (distinct from clap's usage-error 2).
    assert_eq!(
        status.code(),
        Some(3),
        "approval-required must exit 3: stderr={stderr}"
    );
    assert_eq!(v["ok"], false);
    assert_eq!(v["result"]["approval_required"], true);
    // Nothing deleted.
    assert!(root.join("target/bin").is_file());
    assert!(root.join("ambiguous.tmp").is_file());
    // No prompt text on stdout.
    assert!(!stdout.contains("[y/N]"));
    assert!(!stdout.contains("(y/n)"));
    assert_clean_json(&stdout);
}

/// The default run (no subcommand) under `--json --dry-run` reports
/// command "clean" and previews without deleting.
#[test]
fn json_default_run_is_clean_command() {
    let root = root_for("default-run");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();

    let home = root_for("home-default-run");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) =
        run_json(["--config", config.to_str().unwrap(), "--json", "--dry-run"]);
    assert!(status.success());
    assert_eq!(v["command"], "clean");
    assert_eq!(v["ok"], true);
    assert_clean_json(&stdout);
}

/// An error path emits a valid JSON document with ok=false and a message,
/// and exits 1.
#[test]
fn json_error_path_emits_document_and_exits_nonzero() {
    let missing = std::env::temp_dir().join("offcut-json-definitely-missing.toml");
    let (status, v, stdout, _stderr) =
        run_json(["--config", missing.to_str().unwrap(), "list", "--json"]);
    assert!(!status.success());
    assert_eq!(status.code(), Some(1));
    assert_eq!(v["ok"], false);
    assert!(v["error"]["message"].is_string());
    assert_clean_json(&stdout);
}

/// `--json` placement: before the subcommand works too (symmetry with the
/// global-flag fix).
#[test]
fn json_before_subcommand_works() {
    let cfg = empty_config();
    let (status, v, stdout, _stderr) = run_json(["--json", "--config", &cfg, "list"]);
    let _ = std::fs::remove_file(&cfg);
    assert!(status.success());
    assert_eq!(v["command"], "list");
    assert_clean_json(&stdout);
}

/// A clean run with no cleanable projects is a no-op: ok=true, exit 0,
/// empty projects array (or all non-cleanable).
#[test]
fn json_clean_no_cleanable_is_noop() {
    let root = root_for("clean-noop");
    init_repo_with_commit(&root);
    // No remote → not cleanable.
    std::fs::write(root.join("junk.tmp"), "junk").unwrap();
    let home = root_for("home-clean-noop");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, v, stdout, _stderr) = run_json([
        "--config",
        config.to_str().unwrap(),
        "clean",
        "--json",
        "--dry-run",
    ]);
    assert!(status.success());
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["approval_required"], false);
    // The non-cleanable project is reported with its status.
    let projects = v["result"]["projects"].as_array().unwrap();
    assert!(projects.iter().all(|p| p["status"] != "cleanable"));
    assert_clean_json(&stdout);
}
