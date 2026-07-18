//! `.devcleanignore` parsing and matching for devclean.
//!
//! Implements gitignore-style ignore semantics layered across two scopes:
//!
//! - A **global** `~/.devcleanignore` that applies to every project (analogous
//!   to git's `core.excludesfile`). Its patterns are anchored at the project
//!   root, so a leading `/` matches top-level entries of the project.
//! - **Per-folder** `.devcleanignore` files anywhere inside a project tree.
//!   Each applies to the subtree rooted at its own directory; a leading `/`
//!   in such a file anchors to that file's directory.
//!
//! Precedence follows gitignore: the closest (deepest, most-specific)
//! `.devcleanignore` wins, and the global file is the weakest layer. Within a
//! single file the last matching pattern wins, and `!` re-includes a path that
//! an earlier pattern excluded — both handled by the underlying `ignore`
//! crate's per-file matcher.
//!
//! Paths are expressed **relative to the project root** throughout this module.
//! Each ignore layer stores its own anchor as a path relative to the project
//! root (the empty path for the project root / global layer, e.g. `sub` for a
//! `.devcleanignore` in `<root>/sub`). Matching a path therefore reduces to
//! "strip the layer's anchor, then run gitignore matching on the remainder",
//! which keeps the matcher pure and side-effect-free apart from reading ignore
//! files at load time. Because the anchor is stripped here, every layer's
//! `Gitignore` is built with an *empty* root — letting the `ignore` crate strip
//! the anchor a second time would make a rule anchored at `sub` also match
//! `sub/sub/...`.
//!
//! A path is ignored when it matches directly **or when any of its parent
//! directories matches**, so protecting `build/` protects everything under it.
//! Callers therefore do not have to prune during a top-down walk the way git
//! does.
//!
//! On Windows, callers may pass paths with either `/` or `\`; separators are
//! normalized to `/` before matching. On Unix a backslash is a legal filename
//! character and is left alone.

use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// One ignore layer: the directory it is anchored at (relative to the project
/// root; the empty path means the project root itself) plus the compiled
/// gitignore matcher for its patterns.
#[derive(Debug, Clone)]
struct Entry {
    root: PathBuf,
    gi: Gitignore,
}

/// A loaded, immutable set of `.devcleanignore` rules for a single project.
///
/// Entries are stored outermost-first (global, then shallowest local, ...,
/// deepest local). Matching iterates innermost-first so the most specific
/// layer takes precedence, exactly like gitignore.
///
/// `root` is the absolute project root the set was loaded for. It is only used
/// to resolve the project-root-relative paths handed to [`IgnoreSet::is_ignored`]
/// when stat-ing them, so matching stays independent of the process's current
/// directory. Every constructor that can produce layers requires one;
/// [`IgnoreSet::empty`] leaves it unset, which is harmless because a set with
/// no layers ignores nothing regardless of the path's kind.
#[derive(Debug, Clone, Default)]
pub struct IgnoreSet {
    root: PathBuf,
    entries: Vec<Entry>,
}

impl IgnoreSet {
    /// Build an empty ignore set (nothing ignored). Useful as a fallback and
    /// for callers that construct sets incrementally.
    pub fn empty() -> Self {
        IgnoreSet {
            root: PathBuf::new(),
            entries: Vec::new(),
        }
    }

    /// Load the ignore set for a project rooted at `project_root`.
    ///
    /// Reads the global `~/.devcleanignore` (if present) and every
    /// `.devcleanignore` found by walking the project tree. Missing files are
    /// silently skipped — an absent ignore file means "no rules", never an
    /// error. Read errors (permissions, IO) are propagated.
    pub fn load(project_root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let global = dirs::home_dir()
            .map(|h| h.join(".devcleanignore"))
            .filter(|p| p.is_file());
        Self::load_with(project_root, global.as_deref())
    }

    /// Same as [`IgnoreSet::load`] but with an explicit global ignore file path
    /// (pass `None` to load no global layer). Used by [`IgnoreSet::load`] and
    /// by tests so they do not depend on the real home directory.
    fn load_with(
        project_root: &Path,
        global: Option<&Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut set = IgnoreSet::empty();
        set.root = absolute_root(project_root);

        // Global layer, anchored at the project root (like core.excludesfile).
        if let Some(g) = global
            && let Some(gi) = build_matcher(g)?
        {
            set.entries.push(Entry {
                root: PathBuf::new(),
                gi,
            });
        }

        // Per-folder layers, collected shallowest-first so that iteration in
        // reverse (deepest-first) yields correct precedence.
        let mut locals: Vec<Entry> = Vec::new();
        collect_local_ignore_files(project_root, project_root, &mut locals)?;
        set.entries.extend(locals);

        Ok(set)
    }

    /// Build an `IgnoreSet` directly from in-memory `(anchor_dir, patterns)`
    /// pairs. Patterns use gitignore syntax. `anchor_dir` is interpreted
    /// relative to `project_root` (use the empty path for the project root
    /// itself). The first pair is the weakest layer; later pairs override
    /// earlier ones. Intended for tests and for callers that assemble ignore
    /// rules without reading ignore files.
    ///
    /// `project_root` plays the same role as in [`IgnoreSet::load`] and need
    /// not exist: no I/O happens here, but [`IgnoreSet::is_ignored`] resolves
    /// against it later, so passing the wrong root makes directory-only rules
    /// (`build/`) stat the wrong place.
    #[allow(dead_code)]
    pub fn from_layers(
        project_root: &Path,
        layers: &[(&Path, &[&str])],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut set = IgnoreSet::empty();
        set.root = absolute_root(project_root);
        for (root, patterns) in layers {
            let mut b = GitignoreBuilder::new(Path::new(""));
            for line in *patterns {
                b.add_line(Some(root.to_path_buf()), line)?;
            }
            let gi = b.build()?;
            set.entries.push(Entry {
                root: root.to_path_buf(),
                gi,
            });
        }
        Ok(set)
    }

    /// Test whether `rel_path` is ignored.
    ///
    /// `rel_path` is interpreted relative to the project root the set was
    /// loaded for. `is_dir` selects directory-only patterns (those with a
    /// trailing `/`); pass `false` when the path is a file or its kind is
    /// unknown. On Windows, separators are normalized to `/` before matching.
    ///
    /// A path matches a layer when the path itself matches or when any of its
    /// parent directories (up to that layer's anchor) matches, so a rule like
    /// `build/` also covers `build/out.o`.
    ///
    /// Layers are consulted innermost-first; the first layer that produces a
    /// definitive `Ignore` or `Whitelist` (`!`) result wins, and no further
    /// layers are consulted. If no layer matches, the path is not ignored.
    ///
    /// An absolute `rel_path` violates the relative-path contract and is
    /// reported as not ignored rather than matched against the wrong anchors.
    pub fn is_ignored_path(&self, rel_path: &Path, is_dir: bool) -> bool {
        let normalized = normalize_separators(rel_path);
        if normalized.has_root() {
            return false;
        }
        for entry in self.entries.iter().rev() {
            let rel = match strip_root(&entry.root, &normalized) {
                Some(r) => r,
                None => continue,
            };
            match entry.gi.matched_path_or_any_parents(rel, is_dir) {
                ignore::Match::None => continue,
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
            }
        }
        false
    }

    /// Convenience wrapper around [`IgnoreSet::is_ignored_path`] that stats
    /// `rel_path` to determine whether it is a directory. The stat resolves
    /// against the project root the set was loaded for, not the process's
    /// current directory. If the path cannot be stat-ed it is treated as a
    /// file. Prefer [`IgnoreSet::is_ignored_path`] when the caller already
    /// knows the kind.
    pub fn is_ignored(&self, rel_path: &Path) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let is_dir = self.root.join(rel_path).is_dir();
        self.is_ignored_path(rel_path, is_dir)
    }

    /// Number of loaded ignore layers (global + per-folder). Mainly useful for
    /// diagnostics and tests.
    #[allow(dead_code)]
    pub fn layer_count(&self) -> usize {
        self.entries.len()
    }
}

/// Resolve a project root to an absolute path without touching the
/// filesystem, so stats in [`IgnoreSet::is_ignored`] cannot drift with the
/// process's current directory. Falls back to the path as given when it cannot
/// be made absolute (an empty path, or no readable cwd).
fn absolute_root(project_root: &Path) -> PathBuf {
    std::path::absolute(project_root).unwrap_or_else(|_| project_root.to_path_buf())
}

/// Compile a `.devcleanignore`-style file into a `Gitignore`. The matcher is
/// built with an empty root because [`IgnoreSet::is_ignored_path`] strips the
/// layer's anchor itself; giving the builder the anchor too would strip it
/// twice. Returns `Ok(None)` when the file is empty of patterns (blank lines
/// and comments only) so callers can skip storing a no-op layer.
fn build_matcher(file: &Path) -> Result<Option<Gitignore>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(file)?;
    let mut b = GitignoreBuilder::new(Path::new(""));
    let mut saw_pattern = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        b.add_line(Some(file.to_path_buf()), raw)?;
        saw_pattern = true;
    }
    if !saw_pattern {
        return Ok(None);
    }
    Ok(Some(b.build()?))
}

/// Recursively collect every `.devcleanignore` under `dir` (starting at
/// `base`, the project root), recording each with its anchor as a path
/// relative to `base`. Results are pushed in shallowest-first order (pre-order
/// traversal). Symlinks are not followed; unreadable subdirectories and
/// unreadable individual entries are skipped rather than failing the walk.
///
/// `.git` directories are pruned: they never hold user ignore rules and are
/// the single largest source of wasted stats in a real repository. Heavy build
/// directories (`node_modules`, `target`, ...) are deliberately *not* pruned —
/// a `.devcleanignore` inside one is exactly how a user pins something that
/// cleaning would otherwise remove, so skipping them would silently drop
/// protection.
fn collect_local_ignore_files(
    base: &Path,
    dir: &Path,
    out: &mut Vec<Entry>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ignore_file = dir.join(".devcleanignore");
    if ignore_file.is_file() {
        let root = dir
            .strip_prefix(base)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        if let Some(gi) = build_matcher(&ignore_file)? {
            out.push(Entry { root, gi });
        }
    }

    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() && !ft.is_symlink() && entry.file_name() != ".git" {
            collect_local_ignore_files(base, &entry.path(), out)?;
        }
    }
    Ok(())
}

/// Strip `root` (an anchor relative to the project root) from a project-root
/// relative `path`, returning the suffix relative to `root`. Both inputs use
/// `/` separators. Stripping the empty root yields the path unchanged.
fn strip_root<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    if root.as_os_str().is_empty() {
        return Some(path);
    }
    path.strip_prefix(root).ok()
}

/// Normalize path separators to `/` for gitignore matching.
///
/// Windows accepts `\` as a separator, so rewrite it. On Unix a backslash is a
/// legal filename character — rewriting it there would make a file literally
/// named `a\b` match a rule for `a/b` — so the path is passed through as-is.
#[cfg(windows)]
fn normalize_separators(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/"))
}

#[cfg(not(windows))]
fn normalize_separators(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Empty path: the project root anchor. Used as the root for top-level
    /// (global-equivalent) layers in `from_layers` tests.
    const ROOT: &str = "";
    const SUB: &str = "sub";

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    /// Layers rooted at a project root that is never stat-ed: these tests call
    /// `is_ignored_path` with an explicit `is_dir`. Use `layers_rooted` when
    /// the test exercises `is_ignored`.
    fn layers(specs: &[(&str, &[&str])]) -> IgnoreSet {
        layers_rooted(p("."), specs)
    }

    fn layers_rooted(project_root: &Path, specs: &[(&str, &[&str])]) -> IgnoreSet {
        let mapped: Vec<(&Path, &[&str])> = specs.iter().map(|(r, ps)| (p(r), *ps)).collect();
        IgnoreSet::from_layers(project_root, &mapped).unwrap()
    }

    fn unique_dir(label: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        d.push(format!(
            "devclean-ignore-{}-{}-{}",
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
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn empty_set_ignores_nothing() {
        let set = IgnoreSet::empty();
        assert!(!set.is_ignored_path(p("anything"), false));
        assert!(!set.is_ignored_path(p("node_modules/foo"), false));
    }

    #[test]
    fn simple_glob_matches_basenames_anywhere() {
        let set = layers(&[(ROOT, &["*.log"])]);
        assert!(set.is_ignored_path(p("debug.log"), false));
        assert!(set.is_ignored_path(p("a/b/c/error.log"), false));
        assert!(!set.is_ignored_path(p("debug.txt"), false));
    }

    #[test]
    fn double_star_recurses() {
        let set = layers(&[(ROOT, &["**/build"])]);
        assert!(set.is_ignored_path(p("build"), false));
        assert!(set.is_ignored_path(p("a/build"), false));
        assert!(set.is_ignored_path(p("a/b/c/build"), false));
        assert!(!set.is_ignored_path(p("a/buildup"), false));
    }

    #[test]
    fn leading_slash_anchors_to_root() {
        let set = layers(&[(ROOT, &["/foo"])]);
        assert!(set.is_ignored_path(p("foo"), false));
        assert!(!set.is_ignored_path(p("a/foo"), false));
        // `foo/bar` lives under the protected `foo`, so it is protected too.
        assert!(set.is_ignored_path(p("foo/bar"), false));
    }

    #[test]
    fn trailing_slash_matches_directories_only() {
        let set = layers(&[(ROOT, &["build/"])]);
        assert!(set.is_ignored_path(p("build"), true));
        assert!(set.is_ignored_path(p("a/build"), true));
        assert!(!set.is_ignored_path(p("build"), false));
    }

    #[test]
    fn contents_of_ignored_directory_are_ignored() {
        // The cleaning engine consumes is_ignored as "never touch this", so a
        // protected directory must protect everything beneath it.
        let set = layers(&[(ROOT, &["build/", "node_modules"])]);
        assert!(set.is_ignored_path(p("build/out.o"), false));
        assert!(set.is_ignored_path(p("build/deep/nested/out.o"), false));
        assert!(set.is_ignored_path(p("a/build/out.o"), false));
        assert!(set.is_ignored_path(p("node_modules/pkg/index.js"), false));
        assert!(!set.is_ignored_path(p("src/main.rs"), false));
    }

    #[test]
    fn nested_layer_anchor_is_not_stripped_twice() {
        // `/foo` in <root>/sub anchors to `sub` only. Stripping the anchor
        // twice would reduce sub/sub/foo to foo and wrongly match it.
        let set = layers(&[(ROOT, &[]), (SUB, &["/foo"])]);
        assert!(set.is_ignored_path(p("sub/foo"), false));
        assert!(!set.is_ignored_path(p("sub/sub/foo"), false));

        let deep = layers(&[(ROOT, &[]), ("a/b", &["/x"])]);
        assert!(deep.is_ignored_path(p("a/b/x"), false));
        assert!(!deep.is_ignored_path(p("a/b/a/b/x"), false));
    }

    #[test]
    fn absolute_paths_are_not_matched() {
        let set = layers(&[(ROOT, &["*.log"])]);
        assert!(!set.is_ignored_path(p("/tmp/debug.log"), false));
    }

    #[test]
    fn negation_reincludes() {
        let set = layers(&[(ROOT, &["*.log", "!keep.log"])]);
        assert!(set.is_ignored_path(p("debug.log"), false));
        assert!(!set.is_ignored_path(p("keep.log"), false));
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let set = layers(&[(ROOT, &["", "# comment", "# *.log", "target"])]);
        assert!(set.is_ignored_path(p("target"), false));
        assert!(!set.is_ignored_path(p("debug.log"), false));
        assert_eq!(set.layer_count(), 1);
    }

    #[test]
    fn nested_overrides_global() {
        // Root layer says ignore all *.log. A nested layer in `sub` re-includes
        // sub/keep.log. The nested layer, being more specific, must win for
        // paths under `sub`.
        let set = layers(&[(ROOT, &["*.log"]), (SUB, &["!keep.log"])]);
        // Outside sub: root layer applies.
        assert!(set.is_ignored_path(p("debug.log"), false));
        // Inside sub: nested whitelist wins.
        assert!(!set.is_ignored_path(p("sub/keep.log"), false));
        // Other logs inside sub fall through to the root layer.
        assert!(set.is_ignored_path(p("sub/other.log"), false));
    }

    #[test]
    fn nested_ignore_overrides_global_whitelist() {
        let set = layers(&[(ROOT, &["*.bin", "!secret.bin"]), (SUB, &["secret.bin"])]);
        // Top-level secret.bin is whitelisted by the root layer.
        assert!(!set.is_ignored_path(p("secret.bin"), false));
        // Under sub, the nested ignore wins.
        assert!(set.is_ignored_path(p("sub/secret.bin"), false));
    }

    #[test]
    fn anchored_nested_pattern_anchors_to_its_own_dir() {
        // `/foo` in the nested file anchors to `sub`, so it matches sub/foo
        // but not sub/deep/foo.
        let set = layers(&[(ROOT, &[]), (SUB, &["/foo"])]);
        assert!(set.is_ignored_path(p("sub/foo"), false));
        assert!(!set.is_ignored_path(p("sub/deep/foo"), false));
    }

    #[cfg(windows)]
    #[test]
    fn backslash_separators_normalized() {
        let set = layers(&[(ROOT, &["a/b"])]);
        assert!(set.is_ignored_path(p("a\\b"), false));
        assert!(set.is_ignored_path(p("a/b"), false));
    }

    #[cfg(not(windows))]
    #[test]
    fn backslash_is_a_literal_filename_character() {
        // On Unix `a\b` is a single file name, not `a` containing `b`.
        let set = layers(&[(ROOT, &["a/b"])]);
        assert!(set.is_ignored_path(p("a/b"), false));
        assert!(!set.is_ignored_path(p("a\\b"), false));
    }

    #[test]
    fn protected_path_is_reported_ignored() {
        // A "protected path" in devclean terms is one a user has pinned via an
        // ignore rule; cleaning must not remove it. The matcher's contract: a
        // protected path returns is_ignored == true.
        let set = layers(&[(ROOT, &["/important"])]);
        assert!(set.is_ignored_path(p("important"), false));
    }

    #[test]
    fn load_reads_global_and_nested_files() {
        // Use an explicit global file so the test does not depend on the real
        // home directory. The global layer is anchored at the project root.
        let root = unique_dir("load");
        write_file(&root, ".devcleanignore", "*.log\n");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        write_file(&sub, ".devcleanignore", "!keep.log\n");

        let global = write_file(&root, "global.devcleanignore", "*.bak\n");
        let set = IgnoreSet::load_with(&root, Some(&global)).unwrap();
        assert!(set.is_ignored_path(p("debug.log"), false));
        assert!(set.is_ignored_path(p("archive.bak"), false));
        assert!(!set.is_ignored_path(p("sub/keep.log"), false));
        assert!(set.is_ignored_path(p("sub/other.log"), false));
    }

    #[test]
    fn load_handles_missing_ignore_files() {
        let root = unique_dir("empty");
        let set = IgnoreSet::load_with(&root, None).unwrap();
        assert_eq!(set.layer_count(), 0);
        assert!(!set.is_ignored_path(p("anything"), false));
    }

    #[test]
    fn is_ignored_stats_relative_to_the_project_root() {
        // The test process's cwd is the crate directory, never this temp root,
        // so a cwd-relative stat would report `build` as a file and the
        // directory-only rule would not fire.
        let root = unique_dir("stat");
        write_file(&root, ".devcleanignore", "build/\n");
        fs::create_dir_all(root.join("build")).unwrap();

        let set = IgnoreSet::load_with(&root, None).unwrap();
        assert!(set.is_ignored(p("build")));
    }

    #[test]
    fn from_layers_stats_relative_to_its_project_root() {
        // Same cwd-independence guarantee as the `load` path: a set assembled
        // in memory must still resolve directory-only rules against the root
        // it was given, not against the test process's cwd.
        let root = unique_dir("from-layers-stat");
        fs::create_dir_all(root.join("build")).unwrap();

        let set = layers_rooted(&root, &[(ROOT, &["build/"])]);
        assert!(set.is_ignored(p("build")));
        assert!(!set.is_ignored(p("src")));
    }

    #[test]
    fn empty_set_never_ignores_regardless_of_root() {
        let set = IgnoreSet::empty();
        assert!(!set.is_ignored(p("build")));
        assert!(!set.is_ignored(p("anything/at/all")));
    }

    #[test]
    fn load_prunes_git_directories() {
        let root = unique_dir("git");
        write_file(&root, ".git/.devcleanignore", "*.log\n");
        let set = IgnoreSet::load_with(&root, None).unwrap();
        assert_eq!(set.layer_count(), 0);
    }

    #[test]
    fn load_skips_comment_only_files() {
        let root = unique_dir("comments");
        write_file(&root, ".devcleanignore", "# only a comment\n\n");
        let set = IgnoreSet::load_with(&root, None).unwrap();
        assert_eq!(set.layer_count(), 0);
    }
}
