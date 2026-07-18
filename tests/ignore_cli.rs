//! Integration tests for the `devclean ignore` debug subcommand, which is the
//! observable hook for the `.devcleanignore` matcher (issue #3). The matcher
//! module itself is unit-tested in `src/ignore.rs`; these tests exercise the
//! full CLI path: loading real `.devcleanignore` files from a temp project tree
//! and reporting per-path results. They use an explicit `HOME` override so the
//! global `~/.devcleanignore` is never read from the real home directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn devclean() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devclean"))
}

fn unique_dir(label: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    d.push(format!(
        "devclean-ignore-cli-{}-{}-{}",
        label,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_file(dir: &Path, name: &str, contents: &str) {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

/// Run `devclean ignore <path>` inside `cwd`, returning the trimmed stdout.
/// HOME is pointed at an empty temp dir so the child never reads the real
/// `~/.devcleanignore`.
fn run_in(cwd: &Path, path: &str) -> (bool, String) {
    let home = unique_dir("home");
    let result = run_in_with_home(cwd, &home, path);
    let _ = std::fs::remove_dir_all(&home);
    result
}

/// Same as [`run_in`] but with a caller-controlled HOME, so a test can place a
/// global `~/.devcleanignore` where the binary's own `dirs::home_dir()` lookup
/// will find it.
fn run_in_with_home(cwd: &Path, home: &Path, path: &str) -> (bool, String) {
    let out = devclean()
        .current_dir(cwd)
        .env("HOME", home)
        .arg("ignore")
        .arg(path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}\n{stderr}"))
}

#[test]
fn ignore_subcommand_reports_ignored_and_not_ignored() {
    let root = unique_dir("basic");
    write_file(&root, ".devcleanignore", "*.log\n");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    write_file(&sub, ".devcleanignore", "!keep.log\n");

    let (ok, out) = run_in(&root, "debug.log");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "debug.log: ignored");

    let (ok, out) = run_in(&root, "sub/keep.log");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/keep.log: not-ignored");

    let (ok, out) = run_in(&root, "README.md");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "README.md: not-ignored");
}

#[test]
fn ignore_subcommand_nested_precedence_overrides_global_anchoring() {
    let root = unique_dir("anchor");
    // Root-level anchored pattern ignores only top-level `build`.
    write_file(&root, ".devcleanignore", "/build\n");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // Nested file re-includes nothing but tests anchoring: /build in sub
    // matches sub/build, not sub/deep/build.
    write_file(&sub, ".devcleanignore", "/local\n");

    let (ok, out) = run_in(&root, "build");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "build: ignored");

    // Not top-level, so root `/build` does not match.
    let (ok, out) = run_in(&root, "sub/build");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/build: not-ignored");

    // Nested anchored pattern matches within sub.
    let (ok, out) = run_in(&root, "sub/local");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/local: ignored");
}

#[test]
fn ignore_subcommand_reads_the_real_global_ignore_file() {
    // The other tests point HOME at an empty dir, so nothing exercises
    // `IgnoreSet::load`'s own `dirs::home_dir()` lookup of `~/.devcleanignore`.
    // A user's global file is half the feature, so cover the whole precedence
    // chain here: global (weakest) -> project root -> nested -> deepest.
    let home = unique_dir("global-home");
    write_file(&home, ".devcleanignore", "# global rules\n*.bak\n/vendor\n");

    let root = unique_dir("global-project");
    write_file(&root, ".devcleanignore", "*.log\nbuild/\n");
    write_file(&root, "sub/.devcleanignore", "!important.log\n");
    write_file(&root, "sub/deep/.devcleanignore", "important.log\n");
    // `build/` is directory-only, so the path must really be a directory.
    std::fs::create_dir_all(root.join("build/nested")).unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();

    let check = |path: &str, expected: &str| {
        let (ok, out) = run_in_with_home(&root, &home, path);
        assert!(ok, "{out}");
        assert_eq!(out.trim_end_matches('\n'), format!("{path}: {expected}"));
    };

    // Global layer fires for a project that has no rule of its own for it.
    check("archive.bak", "ignored");
    check("vendor", "ignored");
    // Project-root layer.
    check("debug.log", "ignored");
    check("src/main.rs", "not-ignored");
    // A protected directory protects its whole subtree.
    check("build", "ignored");
    check("build/nested/deep.o", "ignored");
    // Nested layer re-includes; the deepest layer ignores it again.
    check("sub/important.log", "not-ignored");
    check("sub/deep/important.log", "ignored");
    // Paths the nested layers say nothing about fall back to the root layer.
    check("sub/other.log", "ignored");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ignore_subcommand_reads_ignore_files_inside_node_modules() {
    // The load walk deliberately does not prune heavy build dirs: a
    // `.devcleanignore` inside one is how a user pins something that cleaning
    // would otherwise remove. Pruning would silently drop that protection.
    let home = unique_dir("nm-home");
    let root = unique_dir("nm-project");
    write_file(&root, "node_modules/.devcleanignore", "/patched-pkg\n");
    std::fs::create_dir_all(root.join("node_modules/patched-pkg")).unwrap();

    let check = |path: &str, expected: &str| {
        let (ok, out) = run_in_with_home(&root, &home, path);
        assert!(ok, "{out}");
        assert_eq!(out.trim_end_matches('\n'), format!("{path}: {expected}"));
    };

    check("node_modules/patched-pkg", "ignored");
    check("node_modules/patched-pkg/index.js", "ignored");
    check("node_modules/junk-pkg", "not-ignored");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ignore_subcommand_rejects_paths_outside_the_project_root() {
    // Reporting "not-ignored" for an unanswerable path would be a false
    // negative in exactly the place one is most dangerous: the caller would
    // read it as "safe to delete".
    let root = unique_dir("outside");

    let (ok, out) = run_in(&root, "/etc/hosts");
    assert!(!ok, "{out}");
    assert!(out.contains("outside the project root"), "{out}");

    let (ok, out) = run_in(&root, "../escape.log");
    assert!(!ok, "{out}");
    assert!(out.contains("escapes the project root"), "{out}");
}

#[test]
fn ignore_subcommand_with_no_ignore_files_reports_not_ignored() {
    let root = unique_dir("none");
    let (ok, out) = run_in(&root, "anything.txt");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "anything.txt: not-ignored");
}
