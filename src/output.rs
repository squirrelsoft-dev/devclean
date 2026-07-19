//! Output formatting for devclean: colored, sorted, gated on TTY.
//!
//! Owns everything about how each line of output reads — labels, ranks,
//! color coding per status, the cleanable indicator, and the TTY-vs-piped
//! color gate. Colors appear only when stdout is a TTY; piped or redirected
//! stdout gets plain text, and setting `NO_COLOR` (any value) or
//! `CLICOLOR=0` disables color even on a TTY.
//!
//! Each formatted row follows the shape `[rank] path — label (reason)`,
//! with the reason omitted when it would merely repeat the label
//! (cleanable/clean rows). The label is color-coded per status so the
//! most-needs-attention rows stand out (dirty statuses in warm tones,
//! cleanable/clean in cool tones); the bold-green label is the cleanable
//! indicator, marking which projects are subjects of the cleaning flow.
//!
//! The `emit_colors` flag lets the formatting functions be unit-tested
//! without needing a real TTY — tests pass `Some(true)` to capture color
//! output and `Some(false)` to verify plain degradation.

use owo_colors::{OwoColorize, Style as OwoStyle};
use std::io::IsTerminal;
use std::path::Path;

use crate::classify::Status;
use crate::interactive::status_reason;

/// Whether stdout is a TTY. Used as the runtime gate for color emission:
/// if stdout is a TTY, each formatted line is colored; otherwise plain.
pub fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Runtime color gate: colored only on a TTY, and only when neither
/// `NO_COLOR` (any value, per the no-color.org convention) nor `CLICOLOR=0`
/// asks for plain output.
fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR").is_some_and(|v| v == "0") {
        return false;
    }
    is_tty()
}

/// Color `text` with `style`, gated by `emit_colors`.
///
/// When `emit_colors` is `None`, uses the runtime gate (TTY plus the
/// `NO_COLOR`/`CLICOLOR` conventions). When `Some(false)` or `Some(true)`
/// uses that directly — the way unit tests verify color behavior without
/// needing a real TTY.
pub fn color(text: &str, style: OwoStyle, emit_colors: Option<bool>) -> String {
    let emit = emit_colors.unwrap_or_else(color_enabled);
    if emit {
        format!("{}", text.style(style))
    } else {
        text.to_string()
    }
}

/// Format one row of the sorted project listing:
/// `[rank] <path> — <label> (<reason>)`, with the reason omitted when it
/// would merely repeat the label (cleanable/clean rows).
///
/// `size` is an optional human-readable reclaimable-size string. Only when
/// the status is `Cleanable` is the size appended — dirty (status 1–4) rows
/// show no savings (they are not yet cleanable), and `Clean` rows show
/// nothing either (no deletable junk). The size uses the `~` form so the
/// reader knows the estimate is approximate (permission-denied paths count
/// as zero rather than aborting the walk).
///
/// The label is color-coded per status (warm for dirty, cool for clean) so
/// the most-needs-attention rows stand out at a glance; the bold-green
/// label is the cleanable indicator, marking which projects are subjects
/// of the interactive clean flow.
///
/// The path is printed as-is (no styling, no escaping) — path strings don't
/// gain a semantic meaning that colors should attach.
pub fn format_project_row(path: &Path, status: Status, emit_colors: Option<bool>, size: Option<&str>) -> String {
    let label_style = status_style(status);
    let label = color(status.label(), label_style, emit_colors);
    let reason = status_reason(status);
    let mut row = format!("  [{}] {} — {}", status.rank(), path.display(), label);
    if reason != status.label() {
        row.push_str(&format!(" ({reason})"));
    }
    if let Some(s) = size {
        // Only carry a savings display on cleanable rows. Dirty rows (1–4)
        // are not cleanable; clean rows have nothing to reclaim.
        if status == Status::Cleanable {
            row.push_str(&format!(" (~{s})"));
        }
    }
    row
}

/// Format a summary line for the sorted listing: "listing: N project(s), sorted by status".
///
/// `cleanable` is the number of status-5 cleanable projects; `size` is the
/// optional aggregate reclaimable-size across all cleanable projects.
/// When both are supplied the summary reads "listing: N project(s), M
/// cleanable, ~X reclaimable"; when only `cleanable` is supplied it reads
/// "listing: N project(s), M cleanable"; when neither is supplied it falls
/// back to the legacy form "listing: N project(s), sorted by status".
///
/// Bold when colors are emitted, plain otherwise.
pub fn format_summary(
    count: usize,
    emit_colors: Option<bool>,
    cleanable: Option<usize>,
    size: Option<&str>,
) -> String {
    let mut header = format!("listing: {} project(s)", count);
    if let (Some(n), Some(s)) = (cleanable, size) {
        header = format!("{header}, {n} cleanable, ~{s} reclaimable");
    } else if let Some(n) = cleanable {
        header = format!("{header}, {n} cleanable");
    } else {
        header = format!("{header}, sorted by status");
    }
    color(&header, OwoStyle::new().bold(), emit_colors)
}

// ---------------------------------------------------------------------------
// Private helpers — keep them as-is so the formatter stays color-agnostic.
// ---------------------------------------------------------------------------

/// Style for each status label, color-coded per status.
///
/// Dirty statuses (1–4) use warm tones: red for the most severe, yellow for
/// the rest. Cleanable and clean use cool tones (green) with bold for the
/// ready-to-clean case so the indicator stands out.
fn status_style(status: Status) -> OwoStyle {
    match status {
        Status::NoGit => OwoStyle::new().red(),
        Status::NoRemote => OwoStyle::new().yellow(),
        Status::Unpushed => OwoStyle::new().yellow(),
        Status::Wip => OwoStyle::new().yellow(),
        Status::Cleanable => OwoStyle::new().green().bold(),
        Status::Clean => OwoStyle::new().green(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_true() -> Option<bool> {
        Some(true)
    }

    fn emit_false() -> Option<bool> {
        Some(false)
    }

    /// Each formatted row contains the project path and status label.
    #[test]
    fn project_row_contains_every_field() {
        let row = format_project_row(Path::new("/tmp/project"), Status::Cleanable, emit_false(), None);
        assert!(row.contains("/tmp/project"), "row: {row}");
        assert!(row.contains("cleanable"), "row: {row}");
    }

    /// The status word appears exactly once per row: the cleanable and clean
    /// reasons merely repeat the label, so they are suppressed — the color
    /// (bold green) is the cleanable indicator, not a repeated word.
    #[test]
    fn label_repeating_reason_is_suppressed() {
        let cleanable =
            format_project_row(Path::new("/tmp/project"), Status::Cleanable, emit_false(), None);
        assert_eq!(
            cleanable.matches("cleanable").count(),
            1,
            "row: {cleanable}"
        );
        assert!(!cleanable.contains("(cleanable)"), "row: {cleanable}");

        let clean = format_project_row(Path::new("/tmp/project"), Status::Clean, emit_false(), None);
        assert_eq!(clean.matches("clean").count(), 1, "row: {clean}");
        assert!(!clean.contains("(clean)"), "row: {clean}");
    }

    /// Non-cleanable rows are formatted with their reason.
    #[test]
    fn non_cleanable_row_has_reason() {
        let row = format_project_row(Path::new("/tmp/project"), Status::NoGit, emit_false(), None);
        assert!(row.contains("not git-initialized"), "row: {row}");
        assert!(!row.contains("(cleanable)"), "row: {row}");
    }

    /// Plain output when `emit_colors = Some(false)` — no ANSI escape codes.
    #[test]
    fn plain_output_when_not_tty() {
        let row = format_project_row(Path::new("/tmp/project"), Status::Cleanable, emit_false(), None);
        assert!(
            !row.contains("\x1b["),
            "plain output should contain no escape codes: {row:?}"
        );
        // Verify the content still shows as plain text.
        assert!(
            row.contains("/tmp/project"),
            "plain output still contains the path"
        );
    }

    /// Color output when `emit_colors = Some(true)` — ANSI escape codes present.
    #[test]
    fn colored_output_when_tty() {
        let row = format_project_row(Path::new("/tmp/project"), Status::Cleanable, emit_true(), None);
        assert!(
            row.contains("\x1b["),
            "colored output should contain escape codes: {row:?}"
        );
    }

    /// Each dirty status gets a warm color (red or yellow) — the label
    /// fragment contains an escape sequence that colors it.
    #[test]
    fn dirty_statuses_get_warm_colors() {
        let no_git = format_project_row(Path::new("/tmp/project"), Status::NoGit, emit_true(), None);
        assert!(no_git.contains("\x1b["), "no-git is red: {no_git:?}");

        let no_remote =
            format_project_row(Path::new("/tmp/project"), Status::NoRemote, emit_true(), None);
        assert!(
            no_remote.contains("\x1b["),
            "no-remote is yellow: {no_remote:?}"
        );
    }

    /// Each clean status gets a cool color (green).
    #[test]
    fn clean_statuses_get_cool_colors() {
        let cleanable =
            format_project_row(Path::new("/tmp/project"), Status::Cleanable, emit_true(), None);
        assert!(
            cleanable.contains("\x1b["),
            "cleanable is green+bold: {cleanable:?}"
        );

        let clean = format_project_row(Path::new("/tmp/project"), Status::Clean, emit_true(), None);
        assert!(clean.contains("\x1b["), "clean is green: {clean:?}");
    }

    /// Summary header is bold (colored) when TTY, plain otherwise.
    #[test]
    fn summary_bold_when_tty_plain_when_not() {
        let colored = format_summary(5, emit_true(), None, None);
        assert!(colored.contains("\x1b["), "summary: {colored:?}");
        let plain = format_summary(5, emit_false(), None, None);
        assert!(!plain.contains("\x1b["), "plain summary: {plain:?}");
    }

    /// Each formatted row has the project path (relative to root) printed.
    #[test]
    fn project_row_uses_absolute_path_when_absolute() {
        // The format_project_row uses `path.display()`, which prints the
        // full path. So even an absolute path is printed in full.
        let row = format_project_row(
            Path::new("/tmp/zz/project"),
            Status::Cleanable,
            emit_false(),
            None,
        );
        assert!(row.contains("/tmp/zz/project"), "row uses full path: {row}");
    }

    /// Each formatted row uses `display()` — the path is shown as the user
    /// would see it. A relative path is printed as-is.
    #[test]
    fn project_row_prints_as_displayed() {
        let row = format_project_row(Path::new("my/project"), Status::Cleanable, emit_false(), None);
        assert!(
            row.contains("my/project"),
            "row prints the path as given: {row}"
        );
    }

    /// Each cleanable row carries its size when a non-empty string is supplied.
    #[test]
    fn cleanable_row_carries_size_when_supplied() {
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_false(),
            Some("2.3 GB"),
        );
        assert!(
            row.contains("~2.3 GB"),
            "cleanable row must carry the savings: {row}"
        );
    }

    /// Each non-cleanable row shows no size — dirty statuses (1–4) are not
    /// cleanable, so no reclaimable display accompanies them.
    #[test]
    fn non_cleanable_row_showes_no_size() {
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Wip,
            emit_false(),
            Some("10 GB"),
        );
        assert!(
            !row.contains("~10 GB"),
            "wip row must not carry savings: {row}"
        );
    }

    /// Each clean row shows no size — nothing is deletable in a clean repo.
    #[test]
    fn clean_row_showes_no_size() {
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Clean,
            emit_false(),
            Some("10 GB"),
        );
        assert!(
            !row.contains("~10 GB"),
            "clean row must not carry savings: {row}"
        );
    }

    /// Each summary carries the cleanable count when supplied.
    #[test]
    fn summary_carries_cleanable_count_when_supplied() {
        let s = format_summary(5, emit_false(), Some(3), None);
        assert!(
            s.contains("3 cleanable"),
            "summary must carry the cleanable count: {s}"
        );
    }

    /// Each summary carries the aggregate reclaimable when both counts
    /// and a size string are supplied.
    #[test]
    fn summary_carries_aggregate_when_both_supplied() {
        let s = format_summary(5, emit_false(), Some(3), Some("12 GB"));
        assert!(
            s.contains("3 cleanable"),
            "summary must carry cleanable count: {s}"
        );
        assert!(
            s.contains("~12 GB reclaimable"),
            "summary must carry the aggregate: {s}"
        );
    }

    /// Each summary falls back to the legacy form when neither cleanable
    /// nor size is supplied.
    #[test]
    fn summary_falls_back_to_legacy_when_neither_supplied() {
        let s = format_summary(5, emit_false(), None, None);
        assert!(
            s.contains("sorted by status"),
            "summary must fall back to legacy: {s}"
        );
    }
}
