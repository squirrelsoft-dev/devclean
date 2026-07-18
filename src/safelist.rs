//! Safe-to-delete catalog for devclean.
//!
//! Maintains a merged list of patterns (each a gitignore-style glob, anchored
//! at the project root) that the cleaning engine treats as safe to auto-delete
//! without surfacing for approval. Patterns are matched as gitignore globs:
//! each is treated as a per-file rule in the safelist's single layer, and
//! because no rule begins with `/`, each pattern matches anywhere in the tree.
//!
//! ## Matching semantics
//!
//! Every pattern is a gitignore glob anchored at the project root (no leading
//! `/`). Each pattern matches its directory-name at any depth: `**/node_modules`
//! matches `node_modules` at the root, `a/node_modules` at depth 1, and so on.
//! Matching a directory name also covers everything beneath it (via the
//! `ignore` crate's parent-match semantics): `**/target` protects any path
//! under any `target` directory found in the tree. Users may extend (not
//! replace) the built-in list via `Config::safe_delete`; arbitrary gitignore
//! patterns are accepted there.
//!
//! ## Ownership
//!
//! - Built-in defaults: this `const` (`BUILT_IN_DEFAULTS`).
//! - Merge logic: `SafeSet::from_config` / `SafeSet::merge`.
//! - Tests: in this module.

use std::path::Path;

use crate::config::Config;
use crate::ignore::IgnoreSet;

/// Project root used by tests and the default layer anchor. In-memory, never
/// stat'd — the safelist only needs the anchor for per-layer prefix stripping,
/// which is the empty path for the top-level layer.
const PROJECT_ROOT_EMPTY: &str = "";

/// Built-in patterns compiled into every safe-to-delete set, regardless of
/// whether the user has a `safe_delete` section. Each entry is a
/// `**/<name>` gitignore glob so it matches the directory name at any depth
/// in the tree.
///
/// Non-exhaustive by design — users extend the list via `Config::safe_delete`
/// (see module docs for semantics). Adding or trimming entries here is fine,
/// but remember each one must be a gitignore glob that behaves correctly with
/// the `ignore` crate's `matched_path_or_any_parents` matcher.
pub const BUILT_IN_DEFAULTS: &[&str] = &[
    "**/node_modules",
    "**/target",
    "**/.next",
    "**/.turbo",
    "**/dist",
    "**/build",
    "**/__pycache__",
    "**/.venv",
    "**/venv",
    "**/.pytest_cache",
    "**/.mypy_cache",
    "**/.gradle",
    "**/bin/obj",
    "**/out",
    "**/coverage",
    "**/.nuxt",
    "**/.svelte-kit",
    "**/.cache",
    "**/.parcel-cache",
];

/// A compiled, immutable safe-to-delete set for one project: the patterns
/// loaded for it (built-ins plus any user-supplied entries), plus a matcher
/// built from those patterns.
///
/// Patterns are stored built-in-first so tools can label each one when
/// listing them (e.g. `--dry-run` display).
#[derive(Debug, Clone)]
pub struct SafeSet {
    #[allow(dead_code)]
    patterns: Vec<String>,
    matcher: IgnoreSet,
}

impl SafeSet {
    /// Build a set from the built-in defaults plus every pattern in the
    /// caller's loaded `Config`'s `safe_delete`. The user's entries are
    /// appended after the built-ins, so an absent `safe_delete` still
    /// produces a full built-in set. `project_root` is where the patterns
    /// apply (used by the underlying gitignore matcher for path anchoring);
    /// it may be any directory the process can reach, but only the paths
    /// handed to [`is_safe_to_delete`](Self::is_safe_to_delete) are
    /// interpreted relative to it.
    ///
    /// A malformed pattern in `safe_delete` (e.g. the reversed range
    /// `[z-a]`) is a user-config error, not a bug: it is returned as an `Err`
    /// so the caller can report it like any other config problem.
    pub fn from_config(
        project_root: &Path,
        cfg: &Config,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::merge(project_root, BUILT_IN_DEFAULTS, &cfg.safe_delete)
    }

    /// Build a set from a slice of built-in patterns plus a slice of
    /// user-supplied patterns. Patterns are passed through to the gitignore
    /// builder as-is; the `**/` prefix is expected on directory-name patterns
    /// so they match at any depth. `project_root` anchors the resulting
    /// matcher for the in-memory layer (`is_ignored_path` resolves the path
    /// against it).
    ///
    /// Errors when any pattern fails to compile as a gitignore glob. The
    /// built-ins are known-good, so in practice this only fires on
    /// user-supplied entries.
    pub fn merge(
        project_root: &Path,
        built_in: &[&str],
        user: &[String],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut patterns: Vec<String> = Vec::with_capacity(built_in.len() + user.len());
        for p in built_in {
            patterns.push(p.to_string());
        }
        for u in user {
            patterns.push(u.clone());
        }
        let pattern_refs: Vec<&str> = patterns.iter().map(|s| s.as_str()).collect();
        let specs: Vec<(&Path, &[&str])> = vec![(Path::new(PROJECT_ROOT_EMPTY), &pattern_refs[..])];
        let matcher = IgnoreSet::from_layers(project_root, &specs)?;
        Ok(SafeSet { patterns, matcher })
    }

    /// Number of patterns in this set. Useful for display (e.g.
    /// `--dry-run` / diagnostic tooling).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Returns `true` when no patterns are active — i.e. the set was built
    /// against an empty built-in slice and an empty user slice.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Every pattern currently active in this set (built-in plus
    /// user-supplied), in built-in-first order.
    #[allow(dead_code)]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Test whether `rel_path`, interpreted relative to the project root the
    /// set was built against, is safe to delete. The matcher delegates to
    /// `IgnoreSet::is_ignored_path` (the `ignore` crate's parent-aware
    /// matcher): matching a directory name also covers everything beneath it.
    ///
    /// `is_dir` selects directory-only patterns (those with a trailing `/`);
    /// pass `true` when the path is a directory or its kind is unknown, since
    /// the catalog patterns are all about directories.
    ///
    /// Prefer [`is_safe`](Self::is_safe) when the caller has not already
    /// determined the path's kind: forcing `is_dir = true` for a path that is
    /// really a file makes a user's directory-only pattern (`build/`) report
    /// a plain file named `build` as safe to delete.
    #[allow(dead_code)]
    pub fn is_safe_to_delete(&self, rel_path: &Path, is_dir: bool) -> bool {
        self.matcher.is_ignored_path(rel_path, is_dir)
    }

    /// Convenience wrapper that stats `rel_path` against the project root
    /// the set was built against and calls `is_safe_to_delete` with the
    /// correct `is_dir` value.
    pub fn is_safe(&self, rel_path: &Path) -> bool {
        self.matcher.is_ignored(rel_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    /// Project root used by tests — never stat'd, just an in-memory anchor.
    const PROJECT_ROOT: &str = "testroot";

    fn set(built_in: &[&str], user: &[String]) -> SafeSet {
        SafeSet::merge(p(PROJECT_ROOT), built_in, user).unwrap()
    }

    /// Every built-in pattern matches itself (as a directory) at the root, at
    /// depth 1, and at depth 2. The parent-match semantics protect everything
    /// under each matched directory.
    #[test]
    fn every_built_in_pattern_matches_as_a_directory() {
        let s = set(BUILT_IN_DEFAULTS, &[]);
        let dirs = [
            "node_modules",
            "target",
            ".next",
            ".turbo",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            "venv",
        ];
        for d in &dirs {
            assert!(
                s.is_safe_to_delete(p(d), true),
                "built-in pattern does not match {d}"
            );
            assert!(
                s.is_safe_to_delete(p(&format!("src/{d}")), true),
                "built-in pattern does not match src/{d}"
            );
            assert!(
                s.is_safe_to_delete(p(&format!("a/b/{d}")), true),
                "built-in pattern does not match a/b/{d}"
            );
            assert!(
                s.is_safe_to_delete(p(&format!("a/b/{d}/deep/file.txt")), false),
                "built-in pattern does not protect a file under {d}"
            );
        }
    }

    /// A non-listed path is not safe. This confirms the set does not
    /// treat every path as safe — the contract is precise: patterns match.
    #[test]
    fn not_safe_for_non_listed_path() {
        let s = set(BUILT_IN_DEFAULTS, &[]);
        assert!(!s.is_safe_to_delete(p("src/main.rs"), true));
        assert!(!s.is_safe_to_delete(p("README.md"), true));
        assert!(!s.is_safe_to_delete(p("Cargo.toml"), true));
        assert!(!s.is_safe_to_delete(p("config.json"), true));
    }

    /// A config-added pattern is recognized as safe after merge. The
    /// contract is that `safe_delete` *extends* the built-in catalog, not
    /// replaces it.
    #[test]
    fn config_addition_is_recognized_as_safe() {
        let user = vec!["**/my_build_artifact".to_string()];
        let s = set(BUILT_IN_DEFAULTS, &user);
        assert!(s.is_safe_to_delete(p("my_build_artifact"), true));
        assert!(s.is_safe_to_delete(p("src/my_build_artifact"), true));
        // Built-ins still match alongside the addition.
        assert!(s.is_safe_to_delete(p("node_modules"), true));
    }

    /// A config-added pattern is not recognized if the user never added it.
    #[test]
    fn unknown_config_pattern_is_not_safe() {
        let user = vec!["**/my_build_artifact".to_string()];
        let s = set(BUILT_IN_DEFAULTS, &user);
        assert!(!s.is_safe_to_delete(p("some_other_dir"), true));
    }

    /// An empty user list still produces the full built-in set.
    #[test]
    fn empty_user_list_still_has_built_ins() {
        let s = set(BUILT_IN_DEFAULTS, &[]);
        assert_eq!(s.len(), BUILT_IN_DEFAULTS.len());
        assert!(s.is_safe_to_delete(p("target"), true));
    }

    /// An empty built-in list plus an empty user list yields an empty set.
    #[test]
    fn empty_set_safe() {
        let s = set(&[], &[]);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(!s.is_safe_to_delete(p("anything"), true));
    }

    /// A malformed user pattern is a reportable error, not a panic: it comes
    /// straight from the user's config, so it has to travel the same path as
    /// every other config problem. Note that the gitignore glob parser is
    /// lenient about most junk (`[unclosed` compiles as a literal) — a
    /// reversed character range and a dangling `\` are the reliable triggers.
    #[test]
    fn malformed_user_pattern_is_an_error() {
        for bad in ["[z-a]", "\\"] {
            let user = vec![bad.to_string()];
            assert!(
                SafeSet::merge(p(PROJECT_ROOT), BUILT_IN_DEFAULTS, &user).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    /// A directory-only user pattern must not match a regular file of the
    /// same name. This is the invariant that forcing `is_dir = true` breaks.
    #[test]
    fn directory_only_pattern_does_not_match_a_file() {
        let user = vec!["**/artifacts/".to_string()];
        let s = set(BUILT_IN_DEFAULTS, &user);
        assert!(s.is_safe_to_delete(p("artifacts"), true));
        assert!(!s.is_safe_to_delete(p("artifacts"), false));
    }
}
