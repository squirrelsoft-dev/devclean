//! Machine-readable JSON output mode (`--json`).
//!
//! Under `--json`, each result-producing command emits exactly one JSON
//! document on stdout and nothing else: no ANSI sequences, no live progress
//! rendering, no prompts, no banner. Diagnostics stay on stderr; exit codes
//! stay meaningful (0 success, 1 error, 2 approval-required). The schema is
//! documented in `docs/json-output.md`.
//!
//! The envelope is shared by every command:
//!
//! ```json
//! { "version": 1, "command": "list", "ok": true, "result": { ... } }
//! ```
//!
//! On error:
//!
//! ```json
//! { "version": 1, "command": "list", "ok": false, "error": { "message": "..." } }
//! ```
//!
//! `--json` never prompts and never broadens deletion authority. A `clean`
//! that would need an interactive approval (no `--force`, no `--dry-run`)
//! returns `approval_required: true`, deletes nothing, and exits 2.

use std::path::PathBuf;

use serde::Serialize;

use crate::classify;
use crate::clean;

/// Schema version. Bump only on a breaking change to the result shape.
pub const VERSION: u32 = 1;

/// The shared envelope. `result` and `error` are mutually exclusive.
#[derive(Debug, Serialize)]
pub struct Envelope<R: Serialize> {
    pub version: u32,
    pub command: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<R>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
}

/// One project row in `list` / `classification`.
#[derive(Debug, Serialize)]
pub struct ProjectRow {
    pub path: PathBuf,
    pub status: &'static str,
    pub rank: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ListResult {
    pub projects: Vec<ProjectRow>,
    pub cleanable_count: usize,
    pub total_reclaimable_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ClassificationResult {
    pub projects: Vec<ProjectRow>,
}

#[derive(Debug, Serialize)]
pub struct DiscoveryProject {
    pub path: PathBuf,
    pub marker: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoveryResult {
    pub projects: Vec<DiscoveryProject>,
}

#[derive(Debug, Serialize)]
pub struct ConfigResult {
    pub config_file: Option<PathBuf>,
    pub default_mode: String,
    pub max_depth: usize,
    pub workspace_roots: Vec<PathBuf>,
    pub safe_delete: Vec<String>,
    pub project_markers: Vec<String>,
    pub force: bool,
    pub dry_run: bool,
    pub verbose: bool,
}

#[derive(Debug, Serialize)]
pub struct IgnoreResult {
    pub path: String,
    pub ignored: bool,
}

#[derive(Debug, Serialize)]
pub struct SafelistResult {
    pub path: String,
    pub safe: bool,
}

#[derive(Debug, Serialize)]
pub struct InitResult {
    pub config_file: PathBuf,
    pub created: bool,
}

/// One enumerated item in a `clean` project result.
#[derive(Debug, Serialize)]
pub struct CleanItemRow {
    pub path: PathBuf,
    pub is_dir: bool,
    pub classification: &'static str,
    pub fate: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CleanProjectRow {
    pub path: PathBuf,
    pub status: &'static str,
    pub approved: bool,
    pub deleted_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    pub items: Vec<CleanItemRow>,
}

#[derive(Debug, Serialize)]
pub struct CleanResult {
    pub approval_required: bool,
    pub projects: Vec<CleanProjectRow>,
}

/// Build a success envelope.
pub fn ok<R: Serialize>(command: &'static str, result: R) -> Envelope<R> {
    Envelope {
        version: VERSION,
        command,
        ok: true,
        result: Some(result),
        error: None,
    }
}

/// Build an error envelope. The result type parameter is carried so the
/// document shape is uniform; `result` is `None`.
pub fn err<R: Serialize>(command: &'static str, message: impl Into<String>) -> Envelope<R> {
    Envelope {
        version: VERSION,
        command,
        ok: false,
        result: None,
        error: Some(ErrorBody {
            message: message.into(),
        }),
    }
}

/// Serialize and print one envelope as a single JSON document followed by a
/// newline. Returns `Ok(())` — a serialization failure is a programming bug.
pub fn print<R: Serialize>(env: &Envelope<R>) {
    let mut out = serde_json::to_string(env).expect("JSON result must serialize");
    out.push('\n');
    print!("{out}");
}

/// The status string for a `classify::Status`, matching `Status::label()`.
pub fn status_label(status: classify::Status) -> &'static str {
    status.label()
}

/// The classification string for a `clean::Classification`.
pub fn classification_label(c: clean::Classification) -> &'static str {
    match c {
        clean::Classification::Protected => "protected",
        clean::Classification::Safe => "safe",
        clean::Classification::Surfaced => "surfaced",
    }
}
