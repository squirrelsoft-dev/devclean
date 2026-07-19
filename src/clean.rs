//! Cleaning engine for devclean (issue #7).
//!
//! For each cleanable (status-5) project, enumerate untracked items,
//! partition them into three classes, surface the ambiguous ones for
//! approval, then delete via `git clean -xfd -e <exclusion globs>` —
//! mirroring the original zshrc approach (build an exclusion list, run
//! `git clean`).
//!
//! ## Classification of each untracked item
//!
//! | class       | condition                       | outcome              |
//! |-------------|---------------------------------|----------------------|
//! | `Protected` | `.devcleanignore` match         | never removed        |
//! | `Safe`      | safe-to-delete catalog match    | auto-removed         |
//! | `Surfaced`  | everything else                 | per-project approval |
//!
//! Approval → exclusion list: surfaced items the user **approves** are removed
//! (left un-excluded); surfaced items the user **does not approve** are added
//! to the exclusions (kept). Then `git clean -xfd -e <globs>` runs, deleting
//! only safe-list + approved items.
//!
//! `--force` auto-approves every surfaced item.
//! `--dry-run` computes and prints what would be deleted; deletes nothing.
//!
//! ## Safety contract
//!
//! devclean only ever deletes safe-list items or items the user approves. It
//! never silently deletes ambiguous items. `.devcleanignored` items are
//! always protected.
//!
//! ## Exclusion list
//!
//! Each exclusion is a single gitignore-style `-e` pattern anchored at the
//! project root with a leading `/` and glob-escaped so it matches the
//! enumerated path literally, matching exactly one enumerated path:
//!
//! - every `Protected` item contributes `/<rel_path>` (kept), and
//! - every un-approved `Surfaced` item contributes `/<rel_path>` (kept).
//!
//! `Safe` and approved-`Surfaced` items contribute nothing, so `git clean`
//! deletes them. Anchoring each exclusion at the root (rather than re-using
//! each `.devcleanignore` layer's source pattern verbatim) is what makes
//! nested-layer rules correct: a `/foo` rule in `<root>/sub/.devcleanignore`
//! protects `sub/foo`, and the exclusion `/sub/foo` excludes exactly that
//! path. Re-using the layer pattern `/foo` verbatim would instead exclude
//! `<root>/foo` — the wrong path. The original zshrc only read the global
//! and root-level ignore files, so it never hit this; the cleaning engine
//! supports nested layers and so anchors each exclusion itself.
//!
//! ## Granularity
//!
//! Untracked items are enumerated via `git ls-files --others --directory -z`
//! (without `--exclude-standard`, deliberately — build junk like
//! `node_modules`/`target/` is gitignored by the *project*, but devclean
//! exists to clean it, so gitignored files must stay visible; the
//! `.devcleanignore` matcher is the sole judge of protection). Each entry is
//! a relative path; directories end with `/`. Empty untracked directories
//! are enumerated too (no `--no-empty-directory`): an empty dir is
//! classified like any other item — a `.devcleanignore`-matching empty dir
//! is `Protected` (excluded, never deleted), a safe-list empty dir is
//! `Safe`, and any other empty dir is `Surfaced` for approval. Omitting
//! empty dirs from classification would let `git clean -xfd` delete them
//! without an exclusion, violating the safety contract.
//!
//! A directory entry is tested against the matcher as a directory first.
//! If the directory itself is protected, it is recorded once as `Protected`
//! (its contents are protected by the parent-match semantics, and one
//! exclusion covers the whole tree). If the directory is safe-to-delete, it
//! is recorded once as `Safe`. Otherwise the directory is **not** protected
//! nor safe as a whole, so it is re-listed at file granularity and each file
//! is classified individually — so content patterns (e.g. `*.js`) inside an
//! untracked directory still protect the files they match. If that
//! re-listing is empty (the directory is untracked and empty), the directory
//! itself is recorded as `Surfaced` so it still gets an exclusion (and is
//! not silently deleted). This is the resolution of issue #6's review fix:
//! `--directory`-only enumeration would under-granularity content patterns.
//!
//! ## Deferred findings from #3 (resolved here)
//!
//! 1. **`!` negation semantics:** the matcher already returns
//!    `is_ignored == false` for a `!`-whitelisted path (correct gitignore
//!    behavior). The cleaning engine treats `is_ignored == false` as NOT
//!    protected (deletable subject to the safe-list check and, failing that,
//!    the approval flow), and `is_ignored == true` as protected (never
//!    deleted). A `!`-whitelisted path is therefore eligible for cleaning,
//!    never silently protected.
//! 2. **Absolute-path fail-safe:** for the deletion-gating path, an absolute
//!    path is treated as PROTECTED (never deleted) — fail-safe toward
//!    not-deleting, not fail-open. `enumerate_untracked` classifies any
//!    absolute path as `Protected` before consulting the matcher, so an
//!    absolute path can never be selected for deletion.
//!
//! ## Git backend
//!
//! Shells out to `git` via `std::process::Command`, consistent with the rest
//! of the crate (no `git2` dependency). `git` plumbing is the source of
//! truth.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ignore::IgnoreSet;
use crate::safelist::SafeSet;

/// Classification of one untracked item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// `.devcleanignore` match (or absolute-path fail-safe): protected, never
    /// removed.
    Protected,
    /// safe-to-delete catalog match: auto-removed (not excluded).
    Safe,
    /// everything else: surfaced for per-project approval.
    Surfaced,
}

/// One untracked item, classified.
#[derive(Debug, Clone)]
pub struct CleanItem {
    /// The path relative to the project root, with no trailing `/` for
    /// directories — check `is_dir` to distinguish a directory from a file.
    pub rel_path: PathBuf,
    /// Whether this item is a directory (git listed it with a trailing `/`).
    pub is_dir: bool,
    /// The classification of this item.
    pub classification: Classification,
}

/// Build the exclusion list (`-e` patterns) for `git clean` from classified
/// items, an approved set, and a force flag.
///
/// Each `Protected` item and each un-approved `Surfaced` item contributes one
/// root-anchored, glob-escaped literal exclusion `/<rel_path>`. `Safe` items and approved
/// (or force-auto-approved) `Surfaced` items contribute nothing, so `git
/// clean` deletes them. See the module docs for why each exclusion is
/// anchored at the root rather than re-using layer source patterns.
#[allow(dead_code)]
pub fn build_exclusions(items: &[CleanItem], approved: &[PathBuf], force: bool) -> Vec<String> {
    let mut exclusions: Vec<String> = Vec::new();
    for item in items {
        match item.classification {
            Classification::Protected => {
                push_anchored(&mut exclusions, &item.rel_path);
            }
            Classification::Safe => {}
            Classification::Surfaced => {
                if force || approved.contains(&item.rel_path) {
                    // Approved — left un-excluded so git clean deletes it.
                } else {
                    push_anchored(&mut exclusions, &item.rel_path);
                }
            }
        }
    }
    exclusions
}

/// Push `/<rel_path>` — a root-anchored gitignore literal excluding exactly
/// this path — unless `rel_path` is empty (defensive; should not occur).
///
/// The path is glob-escaped so it matches literally: gitignore
/// metacharacters (`\`, `*`, `?`, `[`, `]`) are backslash-escaped, and
/// trailing spaces (which gitignore would otherwise trim) are escaped too.
/// Without this, a protected filename containing e.g. `[` would produce a
/// pattern that fails to match, and `git clean` would delete the file.
#[allow(dead_code)]
fn push_anchored(exclusions: &mut Vec<String>, rel_path: &Path) {
    let s = rel_path.to_string_lossy();
    if s.is_empty() {
        return;
    }
    let trimmed = s.trim_end_matches('/');
    let mut escaped = String::with_capacity(trimmed.len() + 1);
    for c in trimmed.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let kept = escaped.trim_end_matches(' ').len();
    let trailing_spaces = escaped.len() - kept;
    escaped.truncate(kept);
    for _ in 0..trailing_spaces {
        escaped.push_str("\\ ");
    }
    exclusions.push(format!("/{escaped}"));
}

/// Run `git -C <project_path> <args>...` and return stdout on success, or
/// `Err` carrying stderr.
///
/// Stdout must be valid UTF-8: a lossy decode would corrupt non-UTF-8
/// filenames so their exclusion patterns could never match on disk, turning
/// a protected item into a deleted one. Refusing to proceed is the fail-safe
/// direction — the whole project's clean aborts and nothing is removed.
fn git_cmd(project_path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(project_path);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("git command failed: {e}"))?;
    if out.status.success() {
        String::from_utf8(out.stdout).map_err(|_| {
            format!(
                "git -C {} {}: output contains non-UTF-8 path(s); refusing to clean this project",
                project_path.display(),
                args.join(" ")
            )
        })
    } else {
        Err(format!(
            "git -C {} {} failed (exit {}): {}",
            project_path.display(),
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// Enumerate untracked items for `project_path` and classify each one.
///
/// Runs `git ls-files --others --directory -z` (no `--exclude-standard`:
/// gitignored-by-project build junk must stay visible so devclean can clean
/// it; `.devcleanignore` is the sole protection judge). Empty untracked
/// directories are included (no `--no-empty-directory`) so they are
/// classified like any other item rather than silently deleted by
/// `git clean -xfd`.
///
/// Per-item precedence:
/// 1. absolute path → `Protected` (fail-safe, never deleted);
/// 2. `.devcleanignore` match → `Protected`;
/// 3. directory that is not protected and not safe → recurse into its files
///    so content patterns (e.g. `*.js`) still match each file inside; if the
///    directory is empty (no files to recurse into), record the directory
///    itself as `Surfaced` so it still gets an exclusion;
/// 4. safe-to-delete catalog match → `Safe`;
/// 5. otherwise → `Surfaced`.
///
/// A protected directory is recorded once (parent-match semantics protect its
/// contents; one exclusion covers the whole tree). A safe directory is
/// recorded once. A neither-protected-nor-safe non-empty directory is
/// expanded to its file-level items; an empty one is recorded as `Surfaced`.
pub fn enumerate_untracked(
    project_path: &Path,
    ignore_set: &IgnoreSet,
    safe_set: &SafeSet,
) -> Result<Vec<CleanItem>, Box<dyn std::error::Error>> {
    let output = git_cmd(project_path, &["ls-files", "--others", "--directory", "-z"])?;

    let mut items: Vec<CleanItem> = Vec::new();
    for entry in output.split('\0').filter(|e| !e.is_empty()) {
        let (rel_path, is_dir) = match entry.strip_suffix('/') {
            Some(dir) => (PathBuf::from(dir), true),
            None => (PathBuf::from(entry), false),
        };

        // 1. Absolute-path fail-safe: never selectable for deletion.
        if rel_path.has_root() {
            items.push(CleanItem {
                rel_path,
                is_dir,
                classification: Classification::Protected,
            });
            continue;
        }

        // 2. .devcleanignore match → protected.
        if ignore_set.is_ignored_path(&rel_path, is_dir) {
            items.push(CleanItem {
                rel_path,
                is_dir,
                classification: Classification::Protected,
            });
            continue;
        }

        // 3. Directory not protected: check safe-list at directory granularity;
        //    if safe, record once. Otherwise recurse to file granularity so
        //    content patterns inside the untracked directory still apply. An
        //    empty untracked directory recurses to nothing — record it as
        //    `Surfaced` so it still gets an exclusion (and is not silently
        //    deleted by `git clean -xfd`).
        if is_dir {
            if safe_set.is_safe_to_delete(&rel_path, true) {
                items.push(CleanItem {
                    rel_path,
                    is_dir,
                    classification: Classification::Safe,
                });
                continue;
            }
            let sub_items = enumerate_subtree_files(project_path, &rel_path, ignore_set, safe_set)?;
            if sub_items.is_empty() {
                items.push(CleanItem {
                    rel_path,
                    is_dir,
                    classification: Classification::Surfaced,
                });
            } else {
                items.extend(sub_items);
            }
            continue;
        }

        // 4. File: safe-to-delete catalog match → safe.
        if safe_set.is_safe_to_delete(&rel_path, false) {
            items.push(CleanItem {
                rel_path,
                is_dir,
                classification: Classification::Safe,
            });
            continue;
        }

        // 5. Surfaced: needs approval.
        items.push(CleanItem {
            rel_path,
            is_dir,
            classification: Classification::Surfaced,
        });
    }
    Ok(items)
}

/// Each untracked file beneath `dir`, enumerated at file granularity and
/// classified individually. `git ls-files --others -z -- <dir>` yields full
/// repo-relative paths, so each result is used directly (not joined onto
/// `dir`). A listing failure aborts the whole project's enumeration (the
/// error propagates), which is fail-safe: nothing is deleted.
fn enumerate_subtree_files(
    project_path: &Path,
    dir: &Path,
    ignore_set: &IgnoreSet,
    safe_set: &SafeSet,
) -> Result<Vec<CleanItem>, Box<dyn std::error::Error>> {
    let output = git_cmd(
        project_path,
        &["ls-files", "--others", "-z", "--", &dir.to_string_lossy()],
    )?;

    let mut items: Vec<CleanItem> = Vec::new();
    for entry in output.split('\0').filter(|e| !e.is_empty()) {
        let rel_path = PathBuf::from(entry);
        // Absolute-path fail-safe, applied per file too.
        if rel_path.has_root() {
            items.push(CleanItem {
                rel_path,
                is_dir: false,
                classification: Classification::Protected,
            });
            continue;
        }
        if ignore_set.is_ignored_path(&rel_path, false) {
            items.push(CleanItem {
                rel_path,
                is_dir: false,
                classification: Classification::Protected,
            });
            continue;
        }
        if safe_set.is_safe_to_delete(&rel_path, false) {
            items.push(CleanItem {
                rel_path,
                is_dir: false,
                classification: Classification::Safe,
            });
            continue;
        }
        items.push(CleanItem {
            rel_path,
            is_dir: false,
            classification: Classification::Surfaced,
        });
    }
    Ok(items)
}

/// Run `git clean -xfd -e <exclusion>...` against `project_path`. Only
/// invoked on the destructive path — dry-run mode never reaches git clean
/// (`clean` returns before calling this and `dry_run` only enumerates).
#[allow(dead_code)]
fn run_git_clean(project_path: &Path, exclusions: &[String]) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(project_path);
    cmd.arg("clean").arg("-xfd");
    for ex in exclusions {
        cmd.arg("-e").arg(ex);
    }
    let out = cmd.output().map_err(|e| format!("git clean failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git clean failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// Clean a project.
///
/// Enumerates untracked items, classifies each, builds the exclusion list
/// from protected items plus un-approved surfaced items, and runs
/// `git clean -xfd -e <globs>` (or dry-run). `force` auto-approves every
/// surfaced item; `dry_run` deletes nothing.
///
/// Returns the surfaced items (those needing approval) for the caller to
/// display. In `force` mode every surfaced item is auto-approved, so the
/// caller can treat an empty returned list as "nothing left to approve";
/// in `dry_run` mode the returned list is the full surfaced set for display.
///
/// `approved` is the set of surfaced item paths the user approved in the
/// interactive flow (#8); pass `&[]` for a pure dry-run preview or `force =
/// true` to auto-approve all.
#[allow(dead_code)]
pub fn clean(
    project_path: &Path,
    ignore_set: &IgnoreSet,
    safe_set: &SafeSet,
    approved: &[PathBuf],
    force: bool,
    dry_run: bool,
) -> Result<Vec<CleanItem>, Box<dyn std::error::Error>> {
    let items = enumerate_untracked(project_path, ignore_set, safe_set)?;
    let exclusions = build_exclusions(&items, approved, force);

    if !dry_run {
        run_git_clean(project_path, &exclusions)?;
    }

    // Return only the surfaced items that still need approval: in `force`
    // mode every surfaced item is auto-approved, so this is empty; in
    // non-force mode it is every surfaced item not in `approved`.
    Ok(items
        .into_iter()
        .filter(|i| {
            i.classification == Classification::Surfaced
                && !force
                && !approved.contains(&i.rel_path)
        })
        .collect())
}

/// Dry-run preview: enumerate and classify untracked items without deleting
/// anything. Returns every item with its classification; the caller prints
/// the report. No `git clean` is invoked.
pub fn dry_run(
    project_path: &Path,
    ignore_set: &IgnoreSet,
    safe_set: &SafeSet,
) -> Result<Vec<CleanItem>, Box<dyn std::error::Error>> {
    enumerate_untracked(project_path, ignore_set, safe_set)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safelist::BUILT_IN_DEFAULTS;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir(label: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        d.push(format!(
            "devclean-clean-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn git_init(root: &Path) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("init")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn git_run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
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

    /// A fresh project root, initialized as a git repo with one committed
    /// file so it has a ref. Each test branches from this.
    fn fixture(label: &str) -> PathBuf {
        let root = unique_dir(label);
        git_init(&root);
        git_run(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_run(&root, &["config", "user.email", "test@test.dev"]);
        git_run(&root, &["config", "user.name", "Test"]);
        write_file(&root, "initial.txt", "initial contents");
        git_run(&root, &["add", "initial.txt"]);
        git_run(&root, &["commit", "-m", "initial"]);
        root
    }

    fn safe_set(root: &Path) -> SafeSet {
        SafeSet::merge(root, BUILT_IN_DEFAULTS, &[]).unwrap()
    }

    /// An untracked file matching a `.devcleanignore` rule is classified
    /// `Protected`.
    #[test]
    fn devcleanignored_file_is_protected() {
        let root = fixture("protected_file");
        write_file(&root, ".devcleanignore", "*.log\n");
        write_file(&root, "debug.log", "junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Protected
                    && i.rel_path.to_string_lossy() == "debug.log"
            }),
            "expected debug.log protected: {:?}",
            items,
        );
    }

    /// An untracked directory matching a `.devcleanignore` rule is recorded
    /// once as `Protected` (its contents are protected by parent-match; one
    /// exclusion covers the whole tree).
    #[test]
    fn devcleanignored_directory_is_protected_once() {
        let root = fixture("protected_dir");
        write_file(&root, ".devcleanignore", "build/\n");
        write_file(&root, "build/out.o", "junk");
        write_file(&root, "build/deep/nested.o", "junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        let protected_dirs: Vec<_> = items
            .iter()
            .filter(|i| i.classification == Classification::Protected)
            .map(|i| i.rel_path.to_string_lossy().to_string())
            .collect();
        assert!(
            protected_dirs.iter().any(|p| p == "build"),
            "expected build protected once: {:?}",
            items,
        );
    }

    /// An untracked safe-list directory (e.g. `target`) is classified `Safe`
    /// and recorded once — auto-removed, not surfaced.
    #[test]
    fn safe_listed_directory_is_safe() {
        let root = fixture("safe_dir");
        write_file(&root, "target/debugbin", "junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Safe && i.rel_path.to_string_lossy() == "target"
            }),
            "expected target safe: {:?}",
            items,
        );
    }

    /// A `!`-whitelisted path is treated as NOT protected: it falls through
    /// to the safe-list check and, failing that, the approval flow
    /// (`Surfaced`). This is the deferred #3 `!`-negation test.
    #[test]
    fn whitelisted_path_is_not_protected() {
        let root = fixture("whitelisted");
        let ignore_set =
            IgnoreSet::from_layers(&root, &[(Path::new(""), &["*.log", "!keep.log"])]).unwrap();
        write_file(&root, "keep.log", "kept");

        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Surfaced
                    && i.rel_path.to_string_lossy() == "keep.log"
            }),
            "whitelisted keep.log should be surfaced, not protected: {:?}",
            items,
        );
    }

    /// Absolute-path fail-safe: an absolute path is never selected for
    /// deletion — it is classified `Protected`. This is the deferred #3
    /// absolute-path test. (In practice `git ls-files` yields repo-relative
    /// paths, so this guards against any caller-side path construction that
    /// produces an absolute path.)
    #[test]
    fn absolute_path_is_never_selected_for_deletion() {
        // Direct unit check: classify an absolute path as protected.
        let abs = Path::new("/tmp/devclean-noise");
        assert!(abs.has_root());
        // The fail-safe lives in enumerate_untracked; verify via
        // build_exclusions that a hand-constructed absolute Protected item is
        // excluded (kept), never deleted.
        let items = vec![CleanItem {
            rel_path: PathBuf::from("/tmp/devclean-noise"),
            is_dir: false,
            classification: Classification::Protected,
        }];
        let excl = build_exclusions(&items, &[], false);
        assert!(
            excl.iter().any(|e| e == "//tmp/devclean-noise"),
            "absolute protected path should be excluded: {:?}",
            excl,
        );
    }

    /// A surfaced item the user disapproves is added to the exclusions (kept);
    /// a surfaced item the user approves (or is force-auto-approved) is left
    /// un-excluded (deleted). A protected item is always excluded; a safe
    /// item is never excluded.
    #[test]
    fn exclusions_split_approved_and_disapproved() {
        let items = vec![
            CleanItem {
                rel_path: PathBuf::from("important.bin"),
                is_dir: false,
                classification: Classification::Protected,
            },
            CleanItem {
                rel_path: PathBuf::from("b.tmp"),
                is_dir: false,
                classification: Classification::Surfaced,
            },
            CleanItem {
                rel_path: PathBuf::from("c.tmp"),
                is_dir: false,
                classification: Classification::Surfaced,
            },
            CleanItem {
                rel_path: PathBuf::from("target"),
                is_dir: true,
                classification: Classification::Safe,
            },
        ];

        // Non-force, no approvals: protected + both surfaced excluded; safe
        // not excluded.
        let excl = build_exclusions(&items, &[], false);
        assert!(excl.iter().any(|e| e == "/important.bin"));
        assert!(excl.iter().any(|e| e == "/b.tmp"));
        assert!(excl.iter().any(|e| e == "/c.tmp"));
        assert!(!excl.iter().any(|e| e == "/target"));

        // Non-force, approve b.tmp: b.tmp not excluded; c.tmp still excluded.
        let excl = build_exclusions(&items, &[PathBuf::from("b.tmp")], false);
        assert!(!excl.iter().any(|e| e == "/b.tmp"));
        assert!(excl.iter().any(|e| e == "/c.tmp"));

        // Force: every surfaced item auto-approved; only protected excluded.
        let excl = build_exclusions(&items, &[], true);
        assert!(excl.iter().any(|e| e == "/important.bin"));
        assert!(!excl.iter().any(|e| e == "/b.tmp"));
        assert!(!excl.iter().any(|e| e == "/c.tmp"));
        assert!(!excl.iter().any(|e| e == "/target"));
    }

    /// Exclusion patterns are glob-escaped so filenames containing gitignore
    /// metacharacters (`*`, `?`, `[`, `]`, `\`) or trailing spaces match
    /// literally instead of being interpreted as globs.
    #[test]
    fn exclusions_escape_glob_metacharacters() {
        let items = vec![
            CleanItem {
                rel_path: PathBuf::from("important [backup].dat"),
                is_dir: false,
                classification: Classification::Protected,
            },
            CleanItem {
                rel_path: PathBuf::from("star*name?.tmp"),
                is_dir: false,
                classification: Classification::Surfaced,
            },
            CleanItem {
                rel_path: PathBuf::from("trailing  "),
                is_dir: false,
                classification: Classification::Protected,
            },
        ];
        let excl = build_exclusions(&items, &[], false);
        assert!(
            excl.iter().any(|e| e == "/important \\[backup\\].dat"),
            "bracket name should be escaped: {excl:?}"
        );
        assert!(
            excl.iter().any(|e| e == "/star\\*name\\?.tmp"),
            "star/question name should be escaped: {excl:?}"
        );
        assert!(
            excl.iter().any(|e| e == "/trailing\\ \\ "),
            "trailing spaces should be escaped: {excl:?}"
        );
    }

    /// End-to-end escaping: a protected file whose name contains glob
    /// metacharacters survives a real `git clean` run. Without escaping the
    /// exclusion pattern would fail to match and git would delete it.
    #[test]
    fn clean_keeps_protected_file_with_glob_metacharacters_in_name() {
        let root = fixture("glob_escape");
        write_file(&root, ".devcleanignore", "important*\n");
        git_run(&root, &["add", ".devcleanignore"]);
        git_run(&root, &["commit", "-m", "ignore rules"]);
        write_file(&root, "important [backup].dat", "keep me");
        write_file(&root, "ambiguous.tmp", "surfaced junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let safe = safe_set(&root);
        clean(&root, &ignore_set, &safe, &[], true, false).unwrap();

        assert!(
            root.join("important [backup].dat").is_file(),
            "protected metacharacter-named file must survive git clean"
        );
        assert!(!root.join("ambiguous.tmp").exists());
    }

    /// Dry-run enumerates and classifies without deleting: an untracked
    /// surfaced file is still present after `dry_run`.
    #[test]
    fn dry_run_does_not_delete() {
        let root = fixture("dry_run");
        write_file(&root, "ambiguous.tmp", "junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = dry_run(&root, &ignore_set, &safe_set(&root)).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Surfaced
                    && i.rel_path.to_string_lossy() == "ambiguous.tmp"
            }),
            "expected ambiguous.tmp surfaced: {:?}",
            items,
        );
        // Nothing deleted: the file is still on disk.
        assert!(root.join("ambiguous.tmp").is_file());
    }

    /// Content-pattern granularity: an untracked directory that is not
    /// protected as a whole still has its files matched against
    /// `.devcleanignore` content patterns. A `*.js` rule protects
    /// `junk/keep.js` inside an untracked `junk/` directory, while
    /// `junk/other.txt` is surfaced.
    #[test]
    fn content_patterns_apply_inside_untracked_directories() {
        let root = fixture("content_patterns");
        write_file(&root, ".devcleanignore", "*.js\n");
        write_file(&root, "junk/keep.js", "js");
        write_file(&root, "junk/other.txt", "txt");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        let by_class = |p: &str| {
            items
                .iter()
                .find(|i| i.rel_path.to_string_lossy() == p)
                .map(|i| i.classification)
        };
        assert_eq!(
            by_class("junk/keep.js"),
            Some(Classification::Protected),
            "expected junk/keep.js protected by *.js: {:?}",
            items,
        );
        assert_eq!(
            by_class("junk/other.txt"),
            Some(Classification::Surfaced),
            "expected junk/other.txt surfaced: {:?}",
            items,
        );
    }

    /// End-to-end: `clean` with `force = true` and `dry_run = false` removes
    /// safe-list and surfaced items, leaves protected items on disk.
    #[test]
    fn clean_force_removes_safe_and_surfaced_keeps_protected() {
        let root = fixture("clean_force");
        write_file(&root, ".devcleanignore", "important.dat\n");
        git_run(&root, &["add", ".devcleanignore"]);
        git_run(&root, &["commit", "-m", "ignore rules"]);
        write_file(&root, "important.dat", "keep me");
        write_file(&root, "target/debugbin", "safe junk");
        write_file(&root, "ambiguous.tmp", "surfaced junk");

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let safe = safe_set(&root);
        let surfaced = clean(&root, &ignore_set, &safe, &[], true, false).unwrap();

        // Force mode: nothing left to approve (every surfaced auto-approved).
        assert!(
            surfaced.is_empty(),
            "force should auto-approve all: {surfaced:?}"
        );
        // Protected kept.
        assert!(root.join("important.dat").is_file());
        // Safe and surfaced removed.
        assert!(!root.join("target").exists());
        assert!(!root.join("ambiguous.tmp").exists());
    }

    /// An empty untracked directory matching `.devcleanignore` is classified
    /// `Protected` (excluded, never deleted) — not silently removed by
    /// `git clean -xfd`. Empty dirs flow through the same classification as
    /// other items (the empty-dir-unclassified-deletion safety fix).
    #[test]
    fn empty_devcleanignored_directory_is_protected() {
        let root = fixture("empty_protected_dir");
        write_file(
            &root,
            ".devcleanignore",
            "keepempty/
",
        );
        git_run(&root, &["add", ".devcleanignore"]);
        git_run(&root, &["commit", "-m", "ignore rules"]);
        fs::create_dir_all(root.join("keepempty")).unwrap();

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let items = enumerate_untracked(&root, &ignore_set, &safe_set(&root)).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Protected
                    && i.is_dir
                    && i.rel_path.to_string_lossy() == "keepempty"
            }),
            "empty protected dir should be classified Protected: {:?}",
            items,
        );
    }

    /// An empty untracked directory that is neither protected nor safe is
    /// classified `Surfaced` (needs approval) — not silently deleted. Without
    /// approval it is kept (excluded).
    #[test]
    fn empty_untracked_directory_is_surfaced_and_kept_without_approval() {
        let root = fixture("empty_surfaced_dir");
        fs::create_dir_all(root.join("emptyjunk")).unwrap();

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let safe = safe_set(&root);
        let items = enumerate_untracked(&root, &ignore_set, &safe).unwrap();

        assert!(
            items.iter().any(|i| {
                i.classification == Classification::Surfaced
                    && i.is_dir
                    && i.rel_path.to_string_lossy() == "emptyjunk"
            }),
            "empty untracked dir should be classified Surfaced: {:?}",
            items,
        );

        // Non-force, no approvals: the empty dir is kept (excluded).
        let surfaced = clean(&root, &ignore_set, &safe, &[], false, false).unwrap();
        assert!(
            surfaced
                .iter()
                .any(|i| i.rel_path.to_string_lossy() == "emptyjunk"),
            "empty dir should still need approval: {surfaced:?}"
        );
        assert!(
            root.join("emptyjunk").is_dir(),
            "un-approved empty dir must not be deleted"
        );
    }

    /// End-to-end empty-dir safety: `clean` with `force = true` removes an
    /// empty surfaced directory but keeps an empty `.devcleanignore`-protected
    /// directory.
    #[test]
    fn clean_force_keeps_empty_protected_dir_removes_empty_surfaced_dir() {
        let root = fixture("empty_dir_e2e");
        write_file(
            &root,
            ".devcleanignore",
            "keepempty/
",
        );
        git_run(&root, &["add", ".devcleanignore"]);
        git_run(&root, &["commit", "-m", "ignore rules"]);
        fs::create_dir_all(root.join("keepempty")).unwrap();
        fs::create_dir_all(root.join("emptyjunk")).unwrap();

        let ignore_set = IgnoreSet::load(&root).unwrap();
        let safe = safe_set(&root);
        clean(&root, &ignore_set, &safe, &[], true, false).unwrap();

        assert!(
            root.join("keepempty").is_dir(),
            "empty protected dir must survive git clean"
        );
        assert!(
            !root.join("emptyjunk").exists(),
            "empty surfaced dir should be removed in force mode"
        );
    }
}
