//! Integration tests for the independent-review corrections on PR #40.
//!
//! Covers:
//! - Finding 1: `clean --json` with a sizing/safelist failure on an otherwise
//!   cleanable project must return the no-op result (ok=true,
//!   approval_required=false, exit 0), not approval_required=true with an
//!   empty projects array. The diagnostic stays on stderr; nothing is deleted.
//! - Finding 2: approval-required uses exit 3, distinct from clap's
//!   usage-error exit 2. Malformed CLI usage under --json exits 2 with no
//!   JSON document on stdout; a valid approval-required JSON run exits 3 with
//!   exactly one document.
//! - Finding 4: targeted `clean <PROJECT_PATH> --workspace X --json` emits the
//!   --workspace-ignored notice on stderr with clean JSON stdout; `--verbose`
//!   is accepted before and after every applicable subcommand.
//! - Finding 5: `init <WORKSPACE> --workspace <OTHER>` emits the same
//!   ignored-global-workspace diagnostic for both human and JSON paths and
//!   both argument orders, with clean JSON stdout when --json is set.

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
    let out = d.join(format!("offcut-fix-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    out
}

fn write_config_full(home: &Path, roots: &[&str], depth: usize, extra: &str) -> PathBuf {
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
    if !extra.is_empty() {
        writeln!(f, "{extra}").unwrap();
    }
    p
}

fn write_config(home: &Path, roots: &[&str], depth: usize) -> PathBuf {
    write_config_full(home, roots, depth, "")
}

fn empty_config() -> String {
    let mut dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    dir.push(format!(
        "offcut-fix-empty-{}-{}.toml",
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

fn run_output<I, S>(args: I) -> (std::process::ExitStatus, String, String)
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
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn parse_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.strip_suffix('\n').unwrap_or(stdout)).unwrap_or_else(|e| {
        panic!("stdout must be one JSON document: {e}\n--- stdout ---\n{stdout}")
    })
}

// ---------------------------------------------------------------------------
// Finding 1: sizing/safelist failure on an otherwise-cleanable project
// ---------------------------------------------------------------------------

/// A cleanable project whose safe-set cannot be built (malformed
/// `safe_delete` glob) must not produce `approval_required: true` with an
/// empty projects array. JSON and human paths must agree: this is a no-op
/// (ok=true, approval_required=false, exit 0), with the diagnostic on stderr
/// and nothing deleted.
#[test]
fn json_clean_safelist_failure_is_noop_not_approval_required() {
    let root = root_for("f1-safelist");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-f1-safelist");
    // Malformed glob: the project's own test `malformed_user_pattern_is_an_error`
    // documents `[z-a]` as invalid.
    let config = write_config_full(
        &home,
        &[root.to_str().unwrap()],
        2,
        "safe_delete = [\"[z-a]\"]",
    );
    let (status, stdout, stderr) =
        run_output(["--config", config.to_str().unwrap(), "clean", "--json"]);
    let v = parse_json(&stdout);

    // No-op: exit 0, ok=true, approval_required=false.
    assert_eq!(status.code(), Some(0), "stderr={stderr}");
    assert_eq!(v["ok"], true, "stdout={stdout}");
    assert_eq!(v["result"]["approval_required"], false, "stdout={stdout}");
    // The diagnostic stays on stderr.
    assert!(
        stderr.contains("skipped, could not build safe-to-delete set"),
        "stderr should carry the sizing-failure diagnostic: {stderr}"
    );
    // Nothing deleted.
    assert!(root.join("target/bin").is_file());
    assert!(root.join("ambiguous.tmp").is_file());
    // stdout is exactly one JSON document, no warning prose.
    assert!(!stdout.contains("warning:"));
    assert!(!stdout.contains('\u{1b}'));
}

/// The same malformed-safe_delete config under the human path is a no-op
/// (exit 0) — the JSON path must agree. This is the consistency guard for
/// Finding 1.
#[test]
fn human_clean_safelist_failure_is_noop_exit_zero() {
    let root = root_for("f1-human");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();

    let home = root_for("home-f1-human");
    let config = write_config_full(
        &home,
        &[root.to_str().unwrap()],
        2,
        "safe_delete = [\"[z-a]\"]",
    );
    let (status, _stdout, stderr) =
        run_output(["--config", config.to_str().unwrap(), "clean", "--dry-run"]);
    assert_eq!(status.code(), Some(0), "stderr={stderr}");
    assert!(
        stderr.contains("skipped, could not build safe-to-delete set"),
        "stderr={stderr}"
    );
    assert!(root.join("target/bin").is_file());
}

// ---------------------------------------------------------------------------
// Finding 2: exit 3 for approval-required, distinct from clap usage-error 2
// ---------------------------------------------------------------------------

/// A malformed CLI invocation under `--json` fails inside clap before any
/// JSON is produced: exit 2, no JSON document on stdout.
#[test]
fn malformed_cli_under_json_exits_2_with_no_document() {
    let (status, stdout, _stderr) = run_output(["--json", "list", "--bogus-flag"]);
    assert_eq!(status.code(), Some(2));
    assert!(
        stdout.is_empty(),
        "no JSON document on parse error: {stdout:?}"
    );
}

/// A valid approval-required JSON run exits 3 with exactly one JSON document.
#[test]
fn approval_required_json_exits_3_with_document() {
    let root = root_for("f2-approval");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-f2-approval");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let (status, stdout, _stderr) =
        run_output(["--config", config.to_str().unwrap(), "clean", "--json"]);
    assert_eq!(status.code(), Some(3));
    let v = parse_json(&stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["result"]["approval_required"], true);
    assert!(root.join("target/bin").is_file());
}

// ---------------------------------------------------------------------------
// Finding 4: targeted clean + --workspace + --json; --verbose placement
// ---------------------------------------------------------------------------

/// `clean <PROJECT_PATH> --workspace X --json` emits the --workspace-ignored
/// notice on stderr and keeps stdout a single clean JSON document.
#[test]
fn targeted_clean_workspace_json_separates_notice_and_stdout() {
    let target = root_for("f4-target");
    init_repo_with_commit(&target);
    add_pushed_remote(&target);
    std::fs::create_dir_all(target.join("target")).unwrap();
    std::fs::write(target.join("target/bin"), "safe junk").unwrap();

    // The decoy is a real cleanable project (committed + pushed, with
    // safe-list junk) plus a tracked sentinel file. It is passed via
    // --workspace, which a targeted clean ignores — so the decoy must never
    // be discovered or cleaned. The junk surviving proves it was not
    // cleaned; the sentinel surviving byte-identical proves it was not
    // modified; and the decoy path never appearing in stdout/stderr proves
    // it was not discovered. Each of these fails if targeted clean reaches
    // the decoy.
    let decoy = root_for("f4-decoy");
    init_repo_with_commit(&decoy);
    add_pushed_remote(&decoy);
    std::fs::create_dir_all(decoy.join("target")).unwrap();
    std::fs::write(decoy.join("target/bin"), "decoy safe junk").unwrap();
    let sentinel = decoy.join("sentinel.txt");
    std::fs::write(&sentinel, "decoy-untouched-sentinel").unwrap();
    git_in(&decoy, &["add", "sentinel.txt"]);
    git_in(&decoy, &["commit", "-m", "sentinel"]);
    // Push the sentinel commit so the decoy stays committed+pushed (cleanable):
    // if a regression discovered it, --force would delete its untracked junk.
    git_in(&decoy, &["push", "origin", "main"]);
    let sentinel_bytes = std::fs::read(&sentinel).unwrap();

    let home = root_for("home-f4");
    let config = write_config(&home, &[], 2);
    let (status, stdout, stderr) = run_output([
        "--config",
        config.to_str().unwrap(),
        "clean",
        target.to_str().unwrap(),
        "--workspace",
        decoy.to_str().unwrap(),
        "--json",
        "--force",
    ]);
    assert!(status.success(), "stderr={stderr}");
    // The notice is on stderr, not stdout.
    assert!(
        stderr.contains("--workspace") && stderr.contains("ignored"),
        "stderr should carry the --workspace-ignored notice: {stderr}"
    );
    assert!(
        !stdout.contains("--workspace"),
        "stdout must not carry the notice: {stdout}"
    );
    assert!(!stdout.contains("ignored"), "stdout={stdout}");
    // stdout is exactly one JSON document.
    let v = parse_json(&stdout);
    assert_eq!(v["command"], "clean");
    assert!(!stdout.contains('\u{1b}'));
    // The decoy was never discovered: its path appears in neither stream
    // (the --workspace notice names it on stderr, but the JSON document —
    // the list of projects acted on — must not).
    let decoy_canonical = std::fs::canonicalize(&decoy).unwrap();
    let decoy_str = decoy_canonical.to_string_lossy();
    assert!(
        !stdout.contains(decoy_str.as_ref()),
        "decoy must not appear in the JSON projects list: {stdout}"
    );
    // The targeted project was cleaned.
    assert!(!target.join("target").exists());
    // The decoy was not cleaned: its safe-list junk survives.
    assert!(
        decoy.join("target/bin").is_file(),
        "decoy safe-list junk must survive — it was not discovered/cleaned"
    );
    // The decoy was not modified: the sentinel is byte-identical.
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        sentinel_bytes,
        "decoy sentinel must be byte-identical — it was not touched"
    );
}

/// `--verbose` is accepted before and after every applicable subcommand
/// (it is a newly-global flag, so it belongs in the placement regression).
#[test]
fn verbose_before_and_after_each_subcommand() {
    let cfg = empty_config();
    for sub in ["list", "config", "discovery", "classification"] {
        let before = offcut()
            .args(["--verbose", "--config", &cfg, sub])
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            before.status.success(),
            "--verbose before {sub} must be accepted: stderr={}",
            String::from_utf8_lossy(&before.stderr)
        );
        let after = offcut()
            .args(["--config", &cfg, sub, "--verbose"])
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            after.status.success(),
            "--verbose after {sub} must be accepted: stderr={}",
            String::from_utf8_lossy(&after.stderr)
        );
    }
    for sub in ["ignore", "safelist"] {
        let before = offcut()
            .args(["--verbose", "--config", &cfg, sub, "somepath"])
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            before.status.success(),
            "--verbose before {sub}: stderr={}",
            String::from_utf8_lossy(&before.stderr)
        );
        let after = offcut()
            .args(["--config", &cfg, sub, "somepath", "--verbose"])
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            after.status.success(),
            "--verbose after {sub}: stderr={}",
            String::from_utf8_lossy(&after.stderr)
        );
    }
    let _ = std::fs::remove_file(&cfg);
}

/// `--verbose` is accepted before and after `clean` and `init` too — the two
/// subcommands that need repo/file fixtures and so were not in the no-fixture
/// sweep above. `clean` uses `--dry-run` so nothing is deleted; `init` writes
/// to a throwaway config target. Each assertion fails if the placement
/// ceased to be accepted (clap exits nonzero with a usage error).
#[test]
fn verbose_before_and_after_clean_and_init() {
    // --- clean: a cleanable project, --dry-run so nothing is deleted ---
    let root = root_for("f4-verbose-clean");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    let home = root_for("home-f4-verbose-clean");
    let config = write_config(&home, &[root.to_str().unwrap()], 2);
    let cfg = config.to_str().unwrap().to_string();
    for order in [
        ["--verbose", "--config", cfg.as_str(), "clean", "--dry-run"],
        ["--config", cfg.as_str(), "clean", "--dry-run", "--verbose"],
    ] {
        let out = offcut()
            .args(order)
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clean --verbose must be accepted (order {:?}): stderr={}",
            order,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // --dry-run deleted nothing.
    assert!(
        root.join("target/bin").is_file(),
        "--dry-run must not delete"
    );

    // --- init: a throwaway config target, both orders ---
    let workspace = root_for("f4-verbose-init-ws");
    for order_idx in 0..2 {
        let target = std::env::temp_dir().join(format!(
            "offcut-fix-verbose-init-{}-{}-{}.toml",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            order_idx
        ));
        let _ = std::fs::remove_file(&target);
        let args: Vec<String> = if order_idx == 0 {
            vec![
                "--verbose".to_string(),
                "--config".to_string(),
                target.to_string_lossy().to_string(),
                "init".to_string(),
                workspace.to_string_lossy().to_string(),
            ]
        } else {
            vec![
                "--config".to_string(),
                target.to_string_lossy().to_string(),
                "init".to_string(),
                workspace.to_string_lossy().to_string(),
                "--verbose".to_string(),
            ]
        };
        let out = offcut()
            .args(args.iter().map(String::as_str))
            .env("HOME", "/nonexistent-home")
            .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "init --verbose must be accepted (order {order_idx}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_file(&target);
    }
}

// ---------------------------------------------------------------------------
// Finding 5: init <WORKSPACE> --workspace <OTHER> ignored-global-workspace notice
// ---------------------------------------------------------------------------

/// `init <WORKSPACE> --workspace <OTHER>` (workspace flag after the
/// positional) emits the ignored-global-workspace diagnostic on stderr for
/// the human path and still creates the config with the positional as the
/// workspace root.
#[test]
fn init_with_global_workspace_after_emits_ignored_notice_human() {
    let target = std::env::temp_dir().join(format!(
        "offcut-fix-init-after-{}-{}.toml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&target);
    let real_ws = root_for("f5-real-after");
    let ignored_ws = root_for("f5-ignored-after");
    let (status, stdout, stderr) = run_output([
        "--config",
        target.to_str().unwrap(),
        "init",
        real_ws.to_str().unwrap(),
        "--workspace",
        ignored_ws.to_str().unwrap(),
    ]);
    assert!(status.success(), "stderr={stderr}");
    assert!(
        stderr.contains("--workspace") && stderr.contains("ignored"),
        "human init should warn about the ignored --workspace: {stderr}"
    );
    // The config was created with the positional workspace, not the ignored flag.
    let created = std::fs::read_to_string(&target).unwrap_or_default();
    let _ = std::fs::remove_file(&target);
    assert!(
        created.contains(real_ws.to_str().unwrap()),
        "stdout={stdout}"
    );
    // stdout is the human line, not JSON.
    assert!(!stdout.starts_with('{'));
}

/// Same as above but with `--workspace` before the `init` subcommand — both
/// argument orders emit the notice.
#[test]
fn init_with_global_workspace_before_emits_ignored_notice_human() {
    let target = std::env::temp_dir().join(format!(
        "offcut-fix-init-before-{}-{}.toml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&target);
    let real_ws = root_for("f5-real-before");
    let ignored_ws = root_for("f5-ignored-before");
    let (status, _stdout, stderr) = run_output([
        "--workspace",
        ignored_ws.to_str().unwrap(),
        "--config",
        target.to_str().unwrap(),
        "init",
        real_ws.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&target);
    assert!(status.success(), "stderr={stderr}");
    assert!(
        stderr.contains("--workspace") && stderr.contains("ignored"),
        "human init (--workspace before) should warn: {stderr}"
    );
}

/// `init <WORKSPACE> --workspace <OTHER> --json` emits the notice on stderr
/// and keeps stdout a single clean JSON document with the positional as the
/// workspace root.
#[test]
fn init_with_global_workspace_json_emits_notice_and_clean_stdout() {
    let target = std::env::temp_dir().join(format!(
        "offcut-fix-init-json-{}-{}.toml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&target);
    let real_ws = root_for("f5-real-json");
    let ignored_ws = root_for("f5-ignored-json");
    let (status, stdout, stderr) = run_output([
        "--config",
        target.to_str().unwrap(),
        "init",
        real_ws.to_str().unwrap(),
        "--workspace",
        ignored_ws.to_str().unwrap(),
        "--json",
    ]);
    let _ = std::fs::remove_file(&target);
    assert!(status.success(), "stderr={stderr}");
    assert!(
        stderr.contains("--workspace") && stderr.contains("ignored"),
        "json init should warn about the ignored --workspace on stderr: {stderr}"
    );
    assert!(!stdout.contains("--workspace"), "stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["command"], "init");
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["created"], true);
    assert!(!stdout.contains('\u{1b}'));
}

/// `init <WORKSPACE> --workspace <OTHER> --json` with the flag before the
/// subcommand — both orders emit the notice under JSON.
#[test]
fn init_with_global_workspace_before_json_emits_notice() {
    let target = std::env::temp_dir().join(format!(
        "offcut-fix-init-json-before-{}-{}.toml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&target);
    let real_ws = root_for("f5-real-json-before");
    let ignored_ws = root_for("f5-ignored-json-before");
    let (status, _stdout, stderr) = run_output([
        "--workspace",
        ignored_ws.to_str().unwrap(),
        "--config",
        target.to_str().unwrap(),
        "init",
        real_ws.to_str().unwrap(),
        "--json",
    ]);
    let _ = std::fs::remove_file(&target);
    assert!(status.success(), "stderr={stderr}");
    assert!(
        stderr.contains("--workspace") && stderr.contains("ignored"),
        "json init (--workspace before) should warn: {stderr}"
    );
}
