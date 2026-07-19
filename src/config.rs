//! Configuration loading and merging for devclean.
//!
//! Config is read from a TOML file under the platform config dir, e.g.
//! `~/.config/devclean/config.toml` on Linux,
//! `~/Library/Application Support/devclean/config.toml` on macOS,
//! `%APPDATA%\\devclean\\config.toml` on Windows. A missing file at that
//! *default* location is not an error: built-in defaults are used. A path the
//! user passed explicitly via `--config` is required to exist; that check lives
//! in `main::load_cli_config`, since this module has no notion of where a path
//! came from (`devclean init` is the exception: it creates the file — see
//! `main::run_init`). CLI flags override the loaded config.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Operating mode for delete operations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Prompt before deleting anything (default).
    #[default]
    Interactive,
    /// Delete without prompting.
    Force,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Interactive => write!(f, "interactive"),
            Mode::Force => write!(f, "force"),
        }
    }
}

/// Resolved devclean configuration.
///
/// Fields map 1:1 to the TOML keys documented in the README. Every field has a
/// built-in default, so a partial (or missing) config file still yields a valid
/// `Config`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Paths devclean scans for projects.
    pub workspace_roots: Vec<PathBuf>,
    /// Glob patterns extending the built-in safe-to-delete catalog.
    pub safe_delete: Vec<String>,
    /// Max recursion depth for project discovery.
    pub max_depth: usize,
    /// Marker files used to detect projects.
    pub project_markers: Vec<String>,
    /// Default mode when no `--force`/`--dry-run` flag is given.
    pub default_mode: Mode,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            workspace_roots: Vec::new(),
            safe_delete: Vec::new(),
            max_depth: 4,
            project_markers: vec![
                ".git".to_string(),
                "package.json".to_string(),
                "Cargo.toml".to_string(),
                "go.mod".to_string(),
                "pyproject.toml".to_string(),
                "pom.xml".to_string(),
                "build.gradle".to_string(),
                "*.csproj".to_string(),
            ],
            default_mode: Mode::Interactive,
        }
    }
}

/// CLI flags that override loaded config values.
///
/// Kept separate from the clap `Cli` struct so the config module does not
/// depend on clap. Runtime-only flags (`--dry-run`, `--verbose`) are passed
/// through here so `list` can echo them, but they are not stored in `Config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    /// Extra workspace roots appended to the config's `workspace_roots`.
    pub workspace: Vec<String>,
    /// When set, forces `default_mode = Force`.
    pub force: bool,
    /// Dry-run flag (runtime behavior; echoed by `list`).
    pub dry_run: bool,
    /// Verbose flag (runtime behavior; echoed by `list`).
    pub verbose: bool,
}

impl Config {
    /// Load config from a TOML file. Anything that is not a readable regular
    /// file (missing path, or a directory sitting at the config location) ->
    /// `Ok(None)`. Parse errors are propagated.
    pub fn load(path: &Path) -> Result<Option<Config>, Box<dyn std::error::Error>> {
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(Some(cfg))
    }

    /// Load config from `path`, or fall back to defaults if the file is
    /// missing. Never errors on a missing file.
    pub fn load_or_default(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
        Ok(Self::load(path)?.unwrap_or_default())
    }

    /// Apply CLI overrides on top of this config.
    pub fn apply_overrides(mut self, overrides: &CliOverrides) -> Config {
        for w in &overrides.workspace {
            self.workspace_roots.push(PathBuf::from(w));
        }
        if overrides.force {
            self.default_mode = Mode::Force;
        }
        self
    }
}

/// Return the default config file path for this platform, e.g.
/// `~/.config/devclean/config.toml` on Linux. Returns `None` if the platform
/// config dir cannot be determined.
///
/// The file lives *inside* a `devclean` directory rather than being an
/// extensionless `devclean` file directly under the config dir: the latter
/// collides with the directory users and other tools expect to create there.
pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("devclean").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Write `contents` to a uniquely-named file in the temp dir and return
    /// its path. The caller is responsible for cleanup; tests are short-lived.
    fn tmpfile(contents: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!(
            "devclean-test-{}-{}-{}.toml",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = fs::File::create(&dir).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        dir
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn default_has_builtin_markers_and_depth() {
        let c = Config::default();
        assert_eq!(c.max_depth, 4);
        assert_eq!(c.default_mode, Mode::Interactive);
        assert!(c.workspace_roots.is_empty());
        assert!(c.safe_delete.is_empty());
        assert!(c.project_markers.contains(&".git".to_string()));
        assert!(c.project_markers.contains(&"Cargo.toml".to_string()));
        assert!(c.project_markers.contains(&"*.csproj".to_string()));
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        let cfg = Config::load_or_default(Path::new("/nonexistent/devclean.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn load_parses_full_config() {
        let toml = r#"
workspace_roots = ["/home/me/code", "/home/me/work"]
safe_delete = ["node_modules", "target"]
max_depth = 8
project_markers = [".git", "my.marker"]
default_mode = "force"
"#;
        let f = tmpfile(toml);
        let cfg = Config::load(&f).unwrap().unwrap();
        assert_eq!(
            cfg.workspace_roots,
            vec![
                PathBuf::from("/home/me/code"),
                PathBuf::from("/home/me/work")
            ]
        );
        assert_eq!(cfg.safe_delete, vec!["node_modules", "target"]);
        assert_eq!(cfg.max_depth, 8);
        assert_eq!(cfg.project_markers, vec![".git", "my.marker"]);
        assert_eq!(cfg.default_mode, Mode::Force);
        cleanup(&f);
    }

    #[test]
    fn partial_config_uses_defaults_for_missing_fields() {
        let toml = r#"
max_depth = 2
"#;
        let f = tmpfile(toml);
        let cfg = Config::load(&f).unwrap().unwrap();
        assert_eq!(cfg.max_depth, 2);
        // Defaults preserved for omitted fields.
        assert_eq!(cfg.default_mode, Mode::Interactive);
        assert!(cfg.workspace_roots.is_empty());
        assert_eq!(Config::default().project_markers, cfg.project_markers);
        cleanup(&f);
    }

    #[test]
    fn empty_file_falls_back_to_default() {
        let f = tmpfile("   \n  ");
        let cfg = Config::load(&f).unwrap().unwrap();
        assert_eq!(cfg, Config::default());
        cleanup(&f);
    }

    #[test]
    fn directory_at_config_path_falls_back_to_default() {
        let mut dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!("devclean-test-dir-{}-{}", std::process::id(), n));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), None);
        assert_eq!(Config::load_or_default(&dir).unwrap(), Config::default());
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let f = tmpfile("this is = = not valid toml {{{");
        assert!(Config::load(&f).is_err());
        cleanup(&f);
    }

    #[test]
    fn invalid_mode_value_is_an_error() {
        let f = tmpfile("default_mode = \"yolo\"");
        assert!(Config::load(&f).is_err());
        cleanup(&f);
    }

    #[test]
    fn apply_overrides_appends_workspaces() {
        let base = Config::default();
        let overrides = CliOverrides {
            workspace: vec!["/extra/root".to_string(), "/another".to_string()],
            ..Default::default()
        };
        let cfg = base.apply_overrides(&overrides);
        assert_eq!(
            cfg.workspace_roots,
            vec![PathBuf::from("/extra/root"), PathBuf::from("/another")]
        );
    }

    #[test]
    fn apply_overrides_force_sets_force_mode() {
        let base = Config::default();
        let overrides = CliOverrides {
            force: true,
            ..Default::default()
        };
        let cfg = base.apply_overrides(&overrides);
        assert_eq!(cfg.default_mode, Mode::Force);
    }

    #[test]
    fn apply_overrides_force_does_not_clear_workspaces() {
        let mut base = Config::default();
        base.workspace_roots.push(PathBuf::from("/keep"));
        let overrides = CliOverrides {
            workspace: vec!["/added".to_string()],
            force: true,
            ..Default::default()
        };
        let cfg = base.apply_overrides(&overrides);
        assert_eq!(
            cfg.workspace_roots,
            vec![PathBuf::from("/keep"), PathBuf::from("/added")]
        );
        assert_eq!(cfg.default_mode, Mode::Force);
    }

    #[test]
    fn override_precedence_cli_wins_over_config() {
        let toml = r#"
max_depth = 9
default_mode = "interactive"
"#;
        let f = tmpfile(toml);
        let cfg = Config::load_or_default(&f).unwrap();
        let overrides = CliOverrides {
            force: true,
            ..Default::default()
        };
        let cfg = cfg.apply_overrides(&overrides);
        // Config value preserved where CLI didn't override.
        assert_eq!(cfg.max_depth, 9);
        // CLI override wins for mode.
        assert_eq!(cfg.default_mode, Mode::Force);
        cleanup(&f);
    }

    #[test]
    fn default_config_path_is_under_config_dir() {
        // Smoke test: on dev hosts this should resolve to something non-empty.
        // We only assert the structure, not a specific platform path.
        if let Some(p) = default_config_path() {
            assert!(p.ends_with("devclean/config.toml"));
        }
    }
}
