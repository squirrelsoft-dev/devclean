//! Integration tests for the `devclean classification` subcommand (issue #6).
//!
//! Each test creates a real workspace root on disk, configures it, and runs the
//! real `devclean` binary against it. The printed report is the assertion
//! surface. We use an explicit `HOME` override so the child never picks up the
//! developer's real `~/.config/devclean/config.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Each test gets a fresh workspace root under a per-test temp dir, so the
/// fixtures do not pollute each other and the child never sees the real
/// workspace root the developer is editing in.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-test workspace root — never shared between tests or prior runs.
/// Includes the PID (like `src/classify.rs`'s `unique_dir`) so leftover dirs
/// from earlier test runs at the same counter value do not collide and
/// reinitialize a stale `.git`.
fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "devclean-classify-cli-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&out).unwrap();
    out
}

/// Create a TOML config file under `home` with the given workspace roots.
fn write_config(home: &Path, roots: &[&str], depth: usize) -> PathBuf {
    let p = home.join("config.toml");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    // Quote each root so the array is valid TOML (`workspace_roots = ["/p1", "/p2"]`).
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

/// Run `git -C <root> <args>`, panicking on failure.
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

/// Initialize a git repo at `root` with an initial commit, a configured user,
/// and an initial tracked file. Returns nothing; callers build on top.
fn init_repo_with_commit(root: &Path) {
    git_in(root, &["init"]);
    // Pin the branch name so the fixture does not depend on the ambient
    // `init.defaultBranch`; `add_pushed_remote` assumes `main`. `symbolic-ref`
    // works on every git version, unlike `init -b`.
    git_in(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git_in(root, &["config", "user.email", "test@test.dev"]);
    git_in(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("initial.txt"), "initial").unwrap();
    git_in(root, &["add", "initial.txt"]);
    git_in(root, &["commit", "-m", "initial"]);
}

/// Give `root` a remote (a local bare repo) and push the current branch so the
/// upstream is satisfied (i.e. the repo is "pushed"). Mirrors the fixture
/// pattern in `src/classify.rs` unit tests.
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

/// Run the devclean binary with the given args, returning stdout. The child
/// inherits HOME so the explicit TOML file is found at the right location.
/// Generic over the arg iterable so call sites can pass a plain array literal
/// (`run(["--config", p, "classification"])`) without tripping clippy's
/// needless-borrow lint on a concrete `&[&str]` parameter.
fn run<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = env!("CARGO_BIN_EXE_devclean");
    let child = std::process::Command::new(exe)
        .args(args)
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    String::from_utf8_lossy(&child.stdout).into_owned()
}

/// Test that classification reports each project with its status label.
#[test]
fn classification_reports_each_project_with_status_label() {
    let root = root_for("label-report");
    // A committed git repo with no remote and a modified tracked file.
    // No remote wins precedence (status 2) over WIP (status 4).
    init_repo_with_commit(&root);
    std::fs::write(root.join("initial.txt"), "modified").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    // Point devclean at the explicit config.
    let out = run(["--config", config.to_str().unwrap(), "classification"]);
    assert!(
        out.contains("no-git") || out.contains("no-remote") || out.contains("wip"),
        "expected a status label in output: {out}"
    );
}

/// Test that classification reports no projects when discovery finds nothing.
#[test]
fn classification_reports_nothing_when_discovery_finds_no_projects() {
    let root = root_for("no-projects");
    std::fs::write(root.join("README.md"), "plain").unwrap();
    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--config", config.to_str().unwrap(), "classification"]);
    assert!(out.contains("no projects found"));
}

/// Test that classification sorts by status.
#[test]
fn classification_sorts_by_status() {
    let root = root_for("sort");
    // `sub` is a non-git project (a Cargo.toml marker, no `.git`) → NoGit (1).
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]").unwrap();

    // `root` is a committed+pushed git repo with a modified tracked file → WIP (4).
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::write(root.join("initial.txt"), "modified").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--config", config.to_str().unwrap(), "classification"]);

    // Both states should be reported.
    assert!(
        out.contains("no-git"),
        "expected no-git label in output: {out}"
    );
    assert!(out.contains("wip"), "expected wip label in output: {out}");

    // Sorting by severity: NoGit (rank 1) must appear before WIP (rank 4).
    let no_git_idx = out.find("no-git").unwrap();
    let wip_idx = out.find("wip").unwrap();
    assert!(
        no_git_idx < wip_idx,
        "no-git must sort before wip, got no-git@{no_git_idx} wip@{wip_idx}"
    );
}
