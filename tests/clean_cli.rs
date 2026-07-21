//! Integration tests for the `offcut clean` subcommand (issues #7/#8).
//!
//! Each test creates a real workspace root on disk, configures it, and runs
//! the real `offcut` binary against it. `--dry-run` must never delete
//! anything (even combined with `--force`); a real run — interactive
//! approvals piped over stdin, or `--force` — must delete exactly the safe
//! and approved surfaced items while protected and unapproved items
//! survive. We use an explicit `HOME`/`XDG_CONFIG_HOME` override so the
//! child never picks up the developer's real config or global
//! `~/.offcutignore`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "offcut-clean-cli-{label}-{}-{n}",
        std::process::id()
    ));
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

fn run<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    run_with_stdin(args, "")
}

/// Run the real binary with `input` piped to stdin — the interactive flow
/// reads its approval answers from there. Returns stdout.
fn run_with_stdin<I, S>(args: I, input: &str) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = env!("CARGO_BIN_EXE_offcut");
    let mut child = std::process::Command::new(exe)
        .args(args)
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A cleanable (status-5) project: committed + pushed, with untracked junk
/// that is not offcutignored. `offcut clean --dry-run` prints a preview of
/// each untracked item's classification and deletes nothing.
#[test]
fn clean_dry_run_prints_classifications_without_deleting() {
    let root = root_for("preview");
    init_repo_with_commit(&root);

    // Untracked junk: a safe-list dir, a protected file, an ambiguous file.
    // `.offcutignore` is committed BEFORE the push so the repo stays
    // pushed (cleanable); committing after the push would make it "ahead 1".
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        out.contains("clean:") || out.contains("no projects found"),
        "expected clean header: {out}"
    );
    assert!(
        out.contains("safe-to-delete"),
        "expected safe-to-delete label: {out}"
    );
    assert!(out.contains("protected"), "expected protected label: {out}");
    assert!(out.contains("surfaced"), "expected surfaced label: {out}");

    // Non-destructive: every untracked item is still on disk.
    assert!(root.join("target").exists());
    assert!(root.join("important.dat").is_file());
    assert!(root.join("ambiguous.tmp").is_file());
}

/// `--force --dry-run` previews the force run: each surfaced item is
/// auto-approved for display (`would delete`) and nothing is deleted.
#[test]
fn clean_force_dry_run_auto_approves_each_item() {
    let root = root_for("force");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run([
        "--force",
        "--dry-run",
        "--config",
        config.to_str().unwrap(),
        "clean",
    ]);

    assert!(
        out.contains("would delete"),
        "force preview should mark surfaced as would-delete: {out}"
    );
    assert!(
        !out.contains("would prompt"),
        "force preview auto-approves — nothing left to prompt about: {out}"
    );
    assert!(
        root.join("ambiguous.tmp").is_file(),
        "dry-run must not actually delete: {out}"
    );
}

/// A real interactive run, approvals piped over stdin: the safe item and
/// the approved surfaced item are deleted; the protected item, the
/// declined surfaced item, and tracked files survive.
#[test]
fn clean_interactive_deletes_only_approved_items() {
    let root = root_for("interactive");
    init_repo_with_commit(&root);
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("approve.tmp"), "surfaced, approved").unwrap();
    std::fs::write(root.join("keep.tmp"), "surfaced, declined").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    // Prompts in order: all-cleanup, then the surfaced items in enumeration
    // (sorted) order — approve.tmp ("y"), keep.tmp ("n") — then the
    // per-project confirmation.
    let out = run_with_stdin(
        ["--config", config.to_str().unwrap(), "clean"],
        "y\ny\nn\ny\n",
    );

    assert!(
        !root.join("target").exists(),
        "safe item must be deleted: {out}"
    );
    assert!(
        !root.join("approve.tmp").exists(),
        "approved surfaced item must be deleted: {out}"
    );
    assert!(
        root.join("keep.tmp").is_file(),
        "declined surfaced item must survive: {out}"
    );
    assert!(
        root.join("important.dat").is_file(),
        "protected item must survive: {out}"
    );
    assert!(
        root.join("initial.txt").is_file(),
        "tracked file must survive: {out}"
    );
    assert!(
        out.contains("(deleting)"),
        "real run should report deleting, not would-delete: {out}"
    );
    assert!(
        out.contains("deleted"),
        "expected a post-execution confirmation line: {out}"
    );
}

/// `--force` (no --dry-run) deletes safe and surfaced items without any
/// prompting; protected and tracked files survive.
#[test]
fn clean_force_deletes_without_prompting() {
    let root = root_for("force-real");
    init_repo_with_commit(&root);
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--force", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        !root.join("target").exists(),
        "force must delete safe items: {out}"
    );
    assert!(
        !root.join("ambiguous.tmp").exists(),
        "force must delete surfaced items: {out}"
    );
    assert!(
        root.join("important.dat").is_file(),
        "protected item must survive force: {out}"
    );
    assert!(
        root.join("initial.txt").is_file(),
        "tracked file must survive force: {out}"
    );
    assert!(
        out.contains("deleted"),
        "expected a post-execution confirmation line: {out}"
    );
}

/// `offcut clean --dry-run` skips non-cleanable projects (nothing
/// printed for them unless --verbose) and still deletes nothing.
#[test]
fn clean_dry_run_skips_non_cleanable_projects() {
    let root = root_for("skip");
    init_repo_with_commit(&root);
    // No remote → status 2 (no-remote), not cleanable.
    std::fs::write(root.join("junk.tmp"), "junk").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    // No cleanable project → each project report is printed, but each
    // per-project enumeration is not.
    // The file is untouched.
    assert!(
        root.join("junk.tmp").is_file(),
        "non-cleanable project must not be touched"
    );
    // Without --verbose, non-cleanable projects produce no per-project
    // enumeration lines.
    assert!(
        !out.contains("clean (dry-run):"),
        "non-cleanable project should be skipped silently: {out}"
    );
}

/// Issue #18: the dry-run report row for a cleanable project carries its
/// reclaimable size, and the summary line carries the aggregate.
#[test]
fn clean_dry_run_shows_per_project_and_aggregate_sizes() {
    let root = root_for("size-dryrun");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    // 4 KB of safe-list junk — big enough for a KB-unit size.
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        out.contains("cleanable (~"),
        "cleanable row should carry a size: {out}"
    );
    assert!(
        out.contains("reclaimable"),
        "summary should carry the aggregate reclaimable size: {out}"
    );
}

/// Issue #18: `offcut list` shows the same per-project size on each
/// cleanable row plus the aggregate in the summary line.
#[test]
fn list_shows_per_project_and_aggregate_sizes() {
    let root = root_for("size-list");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--config", config.to_str().unwrap(), "list"]);

    assert!(
        out.contains("reclaimable"),
        "list summary should carry the aggregate reclaimable size: {out}"
    );
    assert!(
        out.contains("cleanable (~"),
        "list cleanable row should carry a size: {out}"
    );
}

// ---------------------------------------------------------------------------
// Project-path targeting: `offcut clean <PROJECT_PATH>` (issue: clean <path>)
// ---------------------------------------------------------------------------

/// A cleanable (status-5) sibling project helper: committed + pushed, with
/// untracked safe-list junk. Returns the project root.
fn cleanable_sibling(label: &str) -> PathBuf {
    let root = root_for(label);
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();
    root
}

/// `offcut clean <PROJECT_PATH>` scopes discovery and deletion strictly to
/// the named project: a neighboring cleanable project under the same
/// workspace root is neither discovered nor cleaned. This is the core
/// regression — path targeting must not clean neighboring projects.
#[test]
fn clean_project_path_does_not_clean_neighbors() {
    let workspace = root_for("ws-target");
    let proj_a = workspace.join("proj-a");
    std::fs::create_dir_all(&proj_a).unwrap();
    init_repo_with_commit(&proj_a);
    add_pushed_remote(&proj_a);
    std::fs::create_dir_all(proj_a.join("target")).unwrap();
    std::fs::write(proj_a.join("target/bin"), "safe junk a").unwrap();

    let proj_b = workspace.join("proj-b");
    std::fs::create_dir_all(&proj_b).unwrap();
    init_repo_with_commit(&proj_b);
    add_pushed_remote(&proj_b);
    std::fs::create_dir_all(proj_b.join("target")).unwrap();
    std::fs::write(proj_b.join("target/bin"), "safe junk b").unwrap();
    std::fs::write(proj_b.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-target");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    // The workspace root is configured, so a non-targeted run would clean
    // both. The targeted run must clean only proj_a.
    let config = write_config(&home.join(".config"), &[workspace.to_str().unwrap()], 3);
    let out = run([
        "--force",
        "--config",
        config.to_str().unwrap(),
        "clean",
        proj_a.to_str().unwrap(),
    ]);

    // proj_a is cleaned.
    assert!(
        !proj_a.join("target").exists(),
        "targeted project's junk must be deleted: {out}"
    );
    // proj_b is untouched — neither its safe junk nor its surfaced item is
    // cleaned, even though `--force` would auto-approve both were it targeted.
    assert!(
        proj_b.join("target/bin").is_file(),
        "neighbor project's safe junk must NOT be cleaned: {out}"
    );
    assert!(
        proj_b.join("ambiguous.tmp").is_file(),
        "neighbor project's surfaced item must NOT be cleaned: {out}"
    );
    // The report mentions only the targeted project. The row carries the
    // canonicalized path, which is what discovery resolves the argument to.
    let listed = std::fs::canonicalize(&proj_a).unwrap();
    assert!(
        out.contains(listed.to_str().unwrap()),
        "report should mention the targeted project: {out}"
    );
    assert!(
        !out.contains("proj-b"),
        "report must not mention the neighbor project: {out}"
    );
}

/// `--dry-run` with a project path previews the targeted project's items
/// and deletes nothing — flag forwarding is behavioral, not just parsing.
#[test]
fn clean_project_path_dry_run_deletes_nothing() {
    let proj = cleanable_sibling("target-dryrun");
    let home = root_for("home-target-dryrun");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    // A workspace root is NOT configured — the path target alone drives the
    // run, proving path targeting works outside any configured workspace.
    let config = write_config(&home.join(".config"), &[], 2);
    let out = run([
        "--dry-run",
        "--config",
        config.to_str().unwrap(),
        "clean",
        proj.to_str().unwrap(),
    ]);

    assert!(
        out.contains("safe-to-delete"),
        "dry-run should preview the targeted project's items: {out}"
    );
    assert!(
        proj.join("target/bin").is_file(),
        "dry-run must not delete the targeted project's junk: {out}"
    );
    assert!(
        proj.join("ambiguous.tmp").is_file(),
        "dry-run must not delete surfaced items: {out}"
    );
}

/// `--force` with a project path deletes the targeted project's safe and
/// surfaced junk while protected and tracked files survive — flag
/// forwarding is behavioral.
#[test]
fn clean_project_path_force_deletes_targeted_project_junk() {
    let proj = root_for("target-force");
    init_repo_with_commit(&proj);
    std::fs::write(proj.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&proj, &["add", ".offcutignore"]);
    git_in(&proj, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&proj);
    std::fs::create_dir_all(proj.join("target")).unwrap();
    std::fs::write(proj.join("target/bin"), "safe junk").unwrap();
    std::fs::write(proj.join("important.dat"), "keep me").unwrap();
    std::fs::write(proj.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home-target-force");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);
    let out = run([
        "--force",
        "--config",
        config.to_str().unwrap(),
        "clean",
        proj.to_str().unwrap(),
    ]);

    assert!(
        !proj.join("target").exists(),
        "force must delete the targeted project's safe junk: {out}"
    );
    assert!(
        !proj.join("ambiguous.tmp").exists(),
        "force must delete the targeted project's surfaced junk: {out}"
    );
    assert!(
        proj.join("important.dat").is_file(),
        "protected file in the targeted project must survive: {out}"
    );
    assert!(
        proj.join("initial.txt").is_file(),
        "tracked file in the targeted project must survive: {out}"
    );
    assert!(
        out.contains("deleted"),
        "expected a post-execution confirmation line: {out}"
    );
}

/// `--force --dry-run` with a project path previews the force run on the
/// targeted project and deletes nothing.
#[test]
fn clean_project_path_force_dry_run_previews_targeted_project() {
    let proj = cleanable_sibling("target-force-dryrun");
    let home = root_for("home-target-force-dryrun");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);
    let out = run([
        "--force",
        "--dry-run",
        "--config",
        config.to_str().unwrap(),
        "clean",
        proj.to_str().unwrap(),
    ]);

    assert!(
        out.contains("would delete"),
        "force+dry-run should mark surfaced as would-delete: {out}"
    );
    assert!(
        !out.contains("would prompt"),
        "force preview auto-approves — nothing left to prompt about: {out}"
    );
    assert!(
        proj.join("target/bin").is_file(),
        "force+dry-run must not delete anything: {out}"
    );
    assert!(
        proj.join("ambiguous.tmp").is_file(),
        "force+dry-run must not delete anything: {out}"
    );
}

/// A relative project path is resolved against the process's current
/// directory. The run operates on exactly that project.
#[test]
fn clean_project_path_relative_resolves_against_cwd() {
    let proj = cleanable_sibling("target-relative");
    let home = root_for("home-target-relative");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);

    // Run the binary with `cwd` set to the project's parent so the relative
    // path is just the project's basename.
    let exe = env!("CARGO_BIN_EXE_offcut");
    let rel = proj.file_name().unwrap().to_string_lossy().to_string();
    let out = std::process::Command::new(exe)
        .args([
            "--force",
            "--config",
            config.to_str().unwrap(),
            "clean",
            &rel,
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .current_dir(proj.parent().unwrap())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "relative path run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !proj.join("target").exists(),
        "relative path must clean the targeted project: {stdout}"
    );
}

/// A non-directory project path is a hard error — fail-fast, mirroring the
/// missing-workspace-root check.
#[test]
fn clean_project_path_non_directory_is_an_error() {
    let tmp = root_for("target-notdir");
    let file = tmp.join("not-a-dir.txt");
    std::fs::write(&file, "hi").unwrap();
    let home = root_for("home-target-notdir");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--dry-run",
            "--config",
            config.to_str().unwrap(),
            "clean",
            file.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "non-directory path must error: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a directory"),
        "error should name the problem: {stderr}"
    );
}

/// A missing project path is a hard error.
#[test]
fn clean_project_path_missing_is_an_error() {
    let home = root_for("home-target-missing");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);
    let missing = std::env::temp_dir().join("offcut-definitely-does-not-exist-xyz");

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--dry-run",
            "--config",
            config.to_str().unwrap(),
            "clean",
            missing.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("not a directory"),
        "error should name the missing path: {stderr}"
    );
}

/// A subdirectory of a git project is not a project — it is a hard error
/// naming the enclosing root, and nothing under it is deleted. Without the
/// check `git -C <subdir>` would answer for the enclosing repo, so a
/// clean+pushed parent would make the subdirectory look cleanable and
/// `--force` would delete untracked files inside it.
#[test]
fn clean_project_path_subdirectory_is_an_error() {
    let proj = cleanable_sibling("target-subdir");
    let sub = proj.join("crates").join("inner");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]").unwrap();
    std::fs::create_dir_all(sub.join("target")).unwrap();
    std::fs::write(sub.join("target/bin"), "junk under a subdir").unwrap();

    let home = root_for("home-target-subdir");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 3);

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--force",
            "--config",
            config.to_str().unwrap(),
            "clean",
            sub.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "a subdirectory target must error: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a project root"),
        "error should say the path is not a project root: {stderr}"
    );
    assert!(
        stderr.contains(
            std::fs::canonicalize(&proj)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ),
        "error should name the enclosing project root: {stderr}"
    );
    assert!(
        sub.join("target/bin").is_file(),
        "nothing under the rejected subdirectory may be deleted: {stderr}"
    );
    assert!(
        proj.join("target/bin").is_file(),
        "the enclosing project must not be cleaned either: {stderr}"
    );
}

/// Path targeting works on a project that is NOT under any configured
/// workspace root — the explicit path alone drives the run. A cleanable
/// project under a configured workspace root is left untouched when the
/// target points elsewhere.
#[test]
fn clean_project_path_ignores_configured_workspace_roots() {
    let other_proj = cleanable_sibling("target-other");
    let target_proj = cleanable_sibling("target-explicit");

    let home = root_for("home-target-ignorews");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    // Configure `other_proj` itself as a workspace root (not its parent —
    // the parent is the shared temp dir, which a non-targeted run would scan
    // wastefully and flakily) so a non-targeted run would still discover it.
    // The targeted run must ignore it.
    let config = write_config(&home.join(".config"), &[other_proj.to_str().unwrap()], 2);
    let out = run([
        "--force",
        "--config",
        config.to_str().unwrap(),
        "clean",
        target_proj.to_str().unwrap(),
    ]);

    assert!(
        !target_proj.join("target").exists(),
        "targeted project must be cleaned: {out}"
    );
    assert!(
        other_proj.join("target/bin").is_file(),
        "configured-workspace project must NOT be cleaned when the target points elsewhere: {out}"
    );
}

/// `offcut --workspace <path> clean <PROJECT_PATH>` emits one concise stderr
/// notice that PROJECT_PATH scopes the run and `--workspace` is ignored, so a
/// mistyped invocation is not mistaken for a wider run. The targeted project
/// is still cleaned; the workspace path is not discovered.
#[test]
fn clean_project_path_warns_when_workspace_flag_combined() {
    let target = cleanable_sibling("target-ws-notice");
    let decoy = cleanable_sibling("target-ws-decoy");
    let home = root_for("home-target-ws-notice");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--force",
            "--workspace",
            decoy.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "clean",
            target.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(out.status.success(), "run should succeed: stderr={stderr}");
    // The notice goes to stderr, names --workspace, and says it is ignored.
    assert!(
        stderr.contains("--workspace"),
        "expected a --workspace notice on stderr: {stderr}"
    );
    assert!(
        stderr.contains("ignored"),
        "notice should say --workspace is ignored: {stderr}"
    );
    // The targeted project is still cleaned.
    assert!(
        !target.join("target").exists(),
        "targeted project must still be cleaned: {stdout}"
    );
    // The decoy workspace path is NOT discovered or cleaned.
    assert!(
        decoy.join("target/bin").is_file(),
        "workspace decoy must NOT be cleaned: {stdout}"
    );
}

/// The `--workspace` notice is NOT emitted when no `PROJECT_PATH` is supplied
/// (the normal clean flow honors `--workspace`), nor when a `PROJECT_PATH` is
/// supplied without `--workspace`. This guards against a noisy false notice.
#[test]
fn clean_project_path_no_notice_without_workspace_flag() {
    let target = cleanable_sibling("target-no-notice");
    let home = root_for("home-target-no-notice");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--force",
            "--config",
            config.to_str().unwrap(),
            "clean",
            target.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "run should succeed: stderr={stderr}");
    assert!(
        !stderr.contains("--workspace"),
        "no --workspace notice when the flag is absent: {stderr}"
    );
    assert!(
        !target.join("target").exists(),
        "targeted project must still be cleaned"
    );
}

/// A path beginning with a literal `~` (the shell did not expand it, e.g. a
/// quoted path or a non-shell invocation) produces the normal "not found"
/// error plus a concise hint pointing at shell tilde expansion — closing the
/// loop on the documented sharp edge without implementing tilde expansion
/// inside offcut.
#[test]
fn clean_project_path_tilde_hint_when_shell_did_not_expand() {
    let home = root_for("home-target-tilde");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--dry-run",
            "--config",
            config.to_str().unwrap(),
            "clean",
            "~/offcut-definitely-does-not-exist-xyz",
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("not a directory"),
        "error should name the missing path: {stderr}"
    );
    assert!(
        stderr.contains("~"),
        "error should echo the literal tilde path: {stderr}"
    );
    assert!(
        stderr.contains("shell"),
        "error should hint at shell expansion: {stderr}"
    );
}

/// A missing path that does NOT start with `~` gets the plain error and no
/// tilde hint — guarding against a spurious hint on ordinary misspelled paths.
#[test]
fn clean_project_path_no_tilde_hint_for_plain_missing_path() {
    let home = root_for("home-target-no-tilde");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[], 2);
    let missing = std::env::temp_dir().join("offcut-plain-missing-xyz");

    let exe = env!("CARGO_BIN_EXE_offcut");
    let out = std::process::Command::new(exe)
        .args([
            "--dry-run",
            "--config",
            config.to_str().unwrap(),
            "clean",
            missing.to_str().unwrap(),
        ])
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("shell"),
        "no tilde hint for a plain missing path: {stderr}"
    );
}
