//! Output formatting for offcut: colored, sorted, gated on TTY.
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
use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::classify::Status;
use crate::clean::{Classification, CleanItem};
use crate::interactive::status_reason;

/// Whether stdout is a TTY. Used as the runtime gate for color emission:
/// if stdout is a TTY, each formatted line is colored; otherwise plain.
pub fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether stderr is a TTY. Interactive prompts and warnings use stderr, so
/// they need their own gate instead of borrowing stdout's redirection state.
pub fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Whether the current terminal can support the richer Offcut UI treatment.
/// `TERM=dumb` is intentionally plain even when a stream is technically a TTY.
pub fn terminal_ui_enabled(stream_is_tty: bool) -> bool {
    terminal_ui_enabled_with(stream_is_tty, std::env::var_os("TERM").as_deref())
}

/// The gate itself, with `TERM` passed in so it is testable without mutating
/// the process environment (which would race every other test in the binary).
fn terminal_ui_enabled_with(stream_is_tty: bool, term: Option<&OsStr>) -> bool {
    stream_is_tty && !term.is_some_and(|v| v == "dumb")
}

/// Whether stdout should receive the rich terminal presentation.
pub fn stdout_terminal_ui_enabled() -> bool {
    terminal_ui_enabled(is_tty())
}

/// Whether stderr should receive the rich interactive prompt presentation.
pub fn stderr_terminal_ui_enabled() -> bool {
    terminal_ui_enabled(stderr_is_tty())
}

/// Current terminal width, used by table renderers. Kept here so presentation
/// code does not duplicate terminal-size fallback rules.
pub fn terminal_width() -> usize {
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        return w as usize;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(80)
}

/// Runtime color gate: colored only on a TTY, and only when neither
/// `NO_COLOR` (any value, per the no-color.org convention) nor `CLICOLOR=0`
/// asks for plain output.
fn color_enabled() -> bool {
    color_enabled_for(is_tty())
}

/// The color gate for one stream: the `NO_COLOR`/`CLICOLOR` conventions
/// apply to every stream; only the TTY test differs per stream.
fn color_enabled_for(stream_is_tty: bool) -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR").is_some_and(|v| v == "0") {
        return false;
    }
    stream_is_tty
}

/// Whether stdout should emit ANSI color under the current environment.
pub fn stdout_color_enabled() -> bool {
    color_enabled()
}

/// Whether stderr should emit ANSI color under the current environment.
/// Interactive prompts and their review panel go to stderr, so they need
/// their own gate rather than borrowing stdout's redirection state.
pub fn stderr_color_enabled() -> bool {
    color_enabled_for(stderr_is_tty())
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

/// Color gate for streams other than stdout. Used by the interactive stderr
/// presentation; tests pass an explicit `emit_colors` value instead.
pub fn color_for_stream(text: &str, style: OwoStyle, emit_colors: bool) -> String {
    if emit_colors {
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
pub fn format_project_row(
    path: &Path,
    status: Status,
    emit_colors: Option<bool>,
    size: Option<&str>,
) -> String {
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
        Status::Cleanable => OwoStyle::new().cyan().bold(),
        Status::Clean => OwoStyle::new().green(),
    }
}

/// Fixed column widths for the wide workspace table. The PROJECT column
/// takes whatever the terminal has left over.
const STATUS_W: usize = 12;
const RECLAIM_W: usize = 10;
const BRANCH_W: usize = 16;
const CHANGED_W: usize = 10;

/// One row in the reference-style workspace project table.
#[derive(Debug, Clone, Copy)]
pub struct ProjectTableRow<'a> {
    pub path: &'a Path,
    pub status: Status,
    pub size: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub changed: Option<&'a str>,
}

/// Format the finished workspace survey state. Wide terminals get the full
/// reference table; narrow terminals degrade to a compact stacked layout that
/// still keeps the same hierarchy and status legend.
pub fn format_project_table(
    rows: &[ProjectTableRow<'_>],
    cleanable_count: usize,
    total_reclaimable: Option<&str>,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let summary = match total_reclaimable {
        Some(size) if cleanable_count > 0 => {
            format!(
                "✓ {} projects · ~{} reclaimable across {} cleanable",
                rows.len(),
                size,
                cleanable_count
            )
        }
        _ => format!("✓ {} projects · {} cleanable", rows.len(), cleanable_count),
    };

    if width >= 84 {
        let project_w = width
            .saturating_sub(STATUS_W + RECLAIM_W + BRANCH_W + CHANGED_W + 14)
            .max(18);
        out.push(format!(
            "{}  {}  {}  {}  {}",
            pad_or_truncate("PROJECT", project_w),
            pad_or_truncate("STATUS", STATUS_W),
            pad_start("RECLAIM", RECLAIM_W),
            pad_or_truncate("BRANCH", BRANCH_W),
            pad_start("CHANGED", CHANGED_W)
        ));
        for row in rows {
            out.push(format!(
                "{}  {}  {}  {}  {}",
                pad_path(&project_label(row.path), project_w),
                // Padding is measured on the plain label and appended
                // outside the escape sequence: `{:<N}` counts chars, so a
                // colored label would blow past N and ragged every column
                // to its right.
                styled_padded(
                    row.status.label(),
                    STATUS_W,
                    status_style(row.status),
                    emit_colors
                ),
                pad_start(row.size.unwrap_or("-"), RECLAIM_W),
                pad_or_truncate(row.branch.unwrap_or("-"), BRANCH_W),
                pad_start(row.changed.unwrap_or("-"), CHANGED_W)
            ));
        }
    } else {
        for row in rows {
            out.push(format!(
                "{} {}",
                color_for_stream("●", status_style(row.status), emit_colors),
                truncate_path_cols(&project_label(row.path), width.saturating_sub(2))
            ));
            out.push(format!(
                "  {} · reclaim {} · branch {} · changed {}",
                colored_status(row.status, emit_colors),
                row.size.unwrap_or("-"),
                row.branch.unwrap_or("-"),
                row.changed.unwrap_or("-")
            ));
        }
    }

    out.push(String::new());
    out.push(color_for_stream(
        &summary,
        OwoStyle::new().green().bold(),
        emit_colors,
    ));
    out.push(format!(
        "run {} to reclaim all, or {} to inspect one",
        color_for_stream("offcut clean", OwoStyle::new().bold(), emit_colors),
        color_for_stream(
            "offcut clean <project>",
            OwoStyle::new().bold(),
            emit_colors
        )
    ));
    out.push(String::new());
    out.push(format!(
        "{} cleanable - safe to remove  {} clean - nothing to trim  {} wip - uncommitted work  {} no-remote/unpushed - not pushed  {} no-git - not a repo",
        color_for_stream("●", status_style(Status::Cleanable), emit_colors),
        color_for_stream("●", status_style(Status::Clean), emit_colors),
        color_for_stream("●", status_style(Status::Wip), emit_colors),
        color_for_stream("●", status_style(Status::NoRemote), emit_colors),
        color_for_stream("●", status_style(Status::NoGit), emit_colors),
    ));
    out
}

/// Column widths for the clean-review item list: the widest classification
/// label (`safe-to-delete`) and the widest fate (`needs approval`).
const LABEL_W: usize = 14;
const FATE_W: usize = 14;

/// One reviewed gitignored path: what it is, and what Offcut will do with it.
///
/// The caller supplies `fate` because only the caller knows which side of the
/// approval the panel is being rendered on — `needs approval` before the
/// prompt, `deleting` / `would delete` / `kept` after it.
#[derive(Debug, Clone, Copy)]
pub struct CleanReviewRow<'a> {
    pub rel_path: &'a Path,
    pub is_dir: bool,
    pub label: &'a str,
    pub fate: &'a str,
}

/// Build the review rows for one project's items, pairing each item's
/// classification label with the fate `fate_of` reports for it. Keeps the two
/// call sites (pre-approval review, post-approval outcome) on one mapping.
pub fn clean_review_rows<'a>(
    items: &'a [CleanItem],
    fate_of: impl Fn(&CleanItem) -> &'static str,
) -> Vec<CleanReviewRow<'a>> {
    items
        .iter()
        .map(|item| CleanReviewRow {
            rel_path: item.rel_path.as_path(),
            is_dir: item.is_dir,
            label: match item.classification {
                Classification::Protected => "protected",
                Classification::Safe => "safe-to-delete",
                Classification::Surfaced => "surfaced",
            },
            fate: fate_of(item),
        })
        .collect()
}

/// Format the project-review panel: the project's branch state and every
/// gitignored path with its classification and its fate.
///
/// The panel carries no question of its own. The confirmation the user answers
/// is the real prompt (`interactive::collect_project_approval`), which is
/// emitted right after this panel on the same stream the answer is read for —
/// a panel-rendered question would be a second, unanswerable copy.
pub fn format_clean_review(
    path: &Path,
    branch_line: &str,
    rows: &[CleanReviewRow<'_>],
    total_size: Option<&str>,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "⟩ analyzing {} · {}",
        color_for_stream(&project_name(path), OwoStyle::new().bold(), emit_colors),
        dim(&path.display().to_string(), emit_colors)
    ));
    out.push(format!(
        "status   {}",
        color_for_stream(
            Status::Cleanable.label(),
            status_style(Status::Cleanable),
            emit_colors
        )
    ));
    out.push(format!("branch   {branch_line}"));
    out.push(String::new());
    out.push(dim("GITIGNORED REVIEW", emit_colors));
    // " ▸ " prefix (4) + item + gap (1) + label + gap (1) + fate.
    let item_width = width.saturating_sub(4 + 1 + LABEL_W + 1 + FATE_W).max(10);
    for row in rows {
        out.push(format!(
            "  ▸ {} {} {}",
            pad_path(
                &format!(
                    "{}{}",
                    row.rel_path.display(),
                    if row.is_dir { "/" } else { "" }
                ),
                item_width
            ),
            styled_padded(row.label, LABEL_W, OwoStyle::new().dimmed(), emit_colors),
            dim(row.fate, emit_colors)
        ));
    }
    out.push(format!(
        "total · {} item(s){}",
        rows.len(),
        total_size.map(|s| format!(" · ~{s}")).unwrap_or_default()
    ));
    out
}

/// Format the blocked clean state for a non-cleanable project.
pub fn format_blocked_project(
    path: &Path,
    status: Status,
    branch_line: &str,
    details: &[String],
    possible_reclaim: Option<&str>,
    emit_colors: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "⟩ analyzing {} · {}",
        color_for_stream(&project_name(path), OwoStyle::new().bold(), emit_colors),
        dim(&path.display().to_string(), emit_colors)
    ));
    out.push(format!(
        "status   {}",
        color_for_stream(status.label(), status_style(status), emit_colors)
    ));
    out.push(format!("branch   {branch_line}"));
    out.push(String::new());
    out.push(color_for_stream(
        &format!("✗ refusing to clean - {}", status_reason(status)),
        status_style(status).bold(),
        emit_colors,
    ));
    out.push("offcut only cleans projects with a clean, pushed tree, so nothing in progress is ever lost.".to_string());
    for detail in details {
        out.push(format!("   {}", dim(detail, emit_colors)));
    }
    out.push(format!(
        "→ commit, push, or initialize as needed, then run {} again",
        color_for_stream(
            &format!("offcut clean {}", path.display()),
            OwoStyle::new().bold(),
            emit_colors
        )
    ));
    if let Some(size) = possible_reclaim {
        out.push(dim(
            &format!("   ~{size} would become reclaimable once the project is cleanable."),
            emit_colors,
        ));
    }
    out
}

/// Format the post-clean success state.
pub fn format_clean_success(
    path: &Path,
    deleted_count: usize,
    reclaimed: Option<&str>,
    emit_colors: bool,
) -> Vec<String> {
    let size = reclaimed
        .map(|s| format!("reclaimed ~{s} - "))
        .unwrap_or_default();
    vec![
        color_for_stream(
            &format!(
                "✓ {size}removed {deleted_count} item(s) from {}",
                project_name(path)
            ),
            OwoStyle::new().green().bold(),
            emit_colors,
        ),
        "gitignored paths only - tracked files untouched.".to_string(),
    ]
}

fn colored_status(status: Status, emit_colors: bool) -> String {
    color_for_stream(status.label(), status_style(status), emit_colors)
}

fn dim(text: &str, emit_colors: bool) -> String {
    color_for_stream(text, OwoStyle::new().dimmed(), emit_colors)
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// The label a project is listed under: its full path, with the home
/// directory abbreviated to `~`.
///
/// A leaf name alone would render `~/work/api` and `~/oss/api` as two
/// identical rows, and could not be pasted back into `offcut clean <path>`,
/// which needs a real path. Over-long labels are truncated from the left
/// (see `pad_path`) so the distinguishing tail always survives.
fn project_label(path: &Path) -> String {
    let display = path.display().to_string();
    let Some(home) = dirs::home_dir() else {
        return display;
    };
    let home = home.display().to_string();
    if home.is_empty() || !display.starts_with(&home) {
        return display;
    }
    let rest = &display[home.len()..];
    if rest.is_empty() {
        return "~".to_string();
    }
    if rest.starts_with(std::path::MAIN_SEPARATOR) {
        return format!("~{rest}");
    }
    display
}

fn pad_or_truncate(text: &str, width: usize) -> String {
    let truncated = truncate_cols(text, width);
    let pad = width.saturating_sub(truncated.width());
    format!("{truncated}{}", " ".repeat(pad))
}

/// Right-align `text` in `width` display columns.
fn pad_start(text: &str, width: usize) -> String {
    let truncated = truncate_cols(text, width);
    let pad = width.saturating_sub(truncated.width());
    format!("{}{truncated}", " ".repeat(pad))
}

/// Left-align a path in `width` display columns, truncating from the *left*
/// so the leaf (the part that distinguishes one row from another) survives.
fn pad_path(text: &str, width: usize) -> String {
    let truncated = truncate_path_cols(text, width);
    let pad = width.saturating_sub(truncated.width());
    format!("{truncated}{}", " ".repeat(pad))
}

/// Style `plain` and pad the result to `width` display columns.
///
/// The padding is measured on the unstyled text and appended outside the
/// escape sequence: a format-spec width (`{:<N}`) counts chars, so a styled
/// cell would overrun `N` by the length of its ANSI codes and shift every
/// column to its right.
fn styled_padded(plain: &str, width: usize, style: OwoStyle, emit_colors: bool) -> String {
    let truncated = truncate_cols(plain, width);
    let pad = width.saturating_sub(truncated.width());
    format!(
        "{}{}",
        color_for_stream(&truncated, style, emit_colors),
        " ".repeat(pad)
    )
}

/// Truncate to `width` display columns keeping the *tail*, with a leading
/// ellipsis. Mirrors the progress writer's path treatment.
fn truncate_path_cols(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    if width <= ellipsis.width() {
        return ellipsis.to_string();
    }
    let keep = width - ellipsis.width();
    let mut cols = 0;
    let mut start = text.len();
    for (i, ch) in text.char_indices().rev() {
        let ch_width = ch.width().unwrap_or(0);
        if cols + ch_width > keep {
            break;
        }
        cols += ch_width;
        start = i;
    }
    format!("{ellipsis}{}", &text[start..])
}

fn truncate_cols(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    if width <= ellipsis.width() {
        return ellipsis.to_string();
    }
    let keep = width - ellipsis.width();
    let mut cols = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if cols + ch_width > keep {
            break;
        }
        cols += ch_width;
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
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
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_false(),
            None,
        );
        assert!(row.contains("/tmp/project"), "row: {row}");
        assert!(row.contains("cleanable"), "row: {row}");
    }

    /// The status word appears exactly once per row: the cleanable and clean
    /// reasons merely repeat the label, so they are suppressed — the color
    /// (bold green) is the cleanable indicator, not a repeated word.
    #[test]
    fn label_repeating_reason_is_suppressed() {
        let cleanable = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_false(),
            None,
        );
        assert_eq!(
            cleanable.matches("cleanable").count(),
            1,
            "row: {cleanable}"
        );
        assert!(!cleanable.contains("(cleanable)"), "row: {cleanable}");

        let clean =
            format_project_row(Path::new("/tmp/project"), Status::Clean, emit_false(), None);
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
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_false(),
            None,
        );
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
        let row = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_true(),
            None,
        );
        assert!(
            row.contains("\x1b["),
            "colored output should contain escape codes: {row:?}"
        );
    }

    /// Each dirty status gets a warm color (red or yellow) — the label
    /// fragment contains an escape sequence that colors it.
    #[test]
    fn dirty_statuses_get_warm_colors() {
        let no_git =
            format_project_row(Path::new("/tmp/project"), Status::NoGit, emit_true(), None);
        assert!(no_git.contains("\x1b["), "no-git is red: {no_git:?}");

        let no_remote = format_project_row(
            Path::new("/tmp/project"),
            Status::NoRemote,
            emit_true(),
            None,
        );
        assert!(
            no_remote.contains("\x1b["),
            "no-remote is yellow: {no_remote:?}"
        );
    }

    /// Each clean status gets a cool color (green).
    #[test]
    fn clean_statuses_get_cool_colors() {
        let cleanable = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_true(),
            None,
        );
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
        let row = format_project_row(
            Path::new("my/project"),
            Status::Cleanable,
            emit_false(),
            None,
        );
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

    #[test]
    fn rich_project_table_has_reference_columns() {
        let rows = vec![
            ProjectTableRow {
                path: Path::new("/workspace/dashboard"),
                status: Status::Cleanable,
                size: Some("1.2 GB"),
                branch: Some("main"),
                changed: Some("2h ago"),
            },
            ProjectTableRow {
                path: Path::new("/workspace/design-system"),
                status: Status::Wip,
                size: Some("540 MB"),
                branch: Some("feat/tokens"),
                changed: Some("12m ago"),
            },
        ];
        let rendered = format_project_table(&rows, 1, Some("1.2 GB"), 100, false).join("\n");
        assert!(rendered.contains("PROJECT"));
        assert!(rendered.contains("STATUS"));
        assert!(rendered.contains("RECLAIM"));
        assert!(rendered.contains("BRANCH"));
        assert!(rendered.contains("CHANGED"));
        assert!(rendered.contains("dashboard"));
        assert!(rendered.contains("cleanable"));
        assert!(rendered.contains("~1.2 GB reclaimable across 1 cleanable"));
    }

    #[test]
    fn narrow_project_table_degrades_to_stacked_rows() {
        let rows = vec![ProjectTableRow {
            path: Path::new("/workspace/dashboard"),
            status: Status::Cleanable,
            size: Some("1.2 GB"),
            branch: Some("main"),
            changed: Some("2h ago"),
        }];
        let rendered = format_project_table(&rows, 1, Some("1.2 GB"), 50, false).join("\n");
        assert!(!rendered.contains("PROJECT"));
        assert!(rendered.contains("dashboard"));
        assert!(rendered.contains("reclaim 1.2 GB"));
        assert!(!rendered.contains("\x1b["));
    }

    /// The review panel states each item's fate but asks nothing: the single
    /// confirmation the user answers is the real prompt on stderr. A question
    /// rendered here would be a second copy nobody reads an answer for.
    #[test]
    fn clean_review_lists_fates_and_asks_nothing() {
        let items = vec![
            CleanItem {
                rel_path: std::path::PathBuf::from("node_modules"),
                is_dir: true,
                classification: Classification::Safe,
            },
            CleanItem {
                rel_path: std::path::PathBuf::from("scratch.tmp"),
                is_dir: false,
                classification: Classification::Surfaced,
            },
        ];
        let rows = clean_review_rows(&items, |item| match item.classification {
            Classification::Protected => "kept",
            Classification::Safe => "will delete",
            Classification::Surfaced => "needs approval",
        });
        let rendered = format_clean_review(
            Path::new("/workspace/dashboard"),
            "main · clean tree · remote ✓ pushed",
            &rows,
            Some("1.2 GB"),
            90,
            false,
        )
        .join("\n");
        assert!(rendered.contains("GITIGNORED REVIEW"));
        assert!(rendered.contains("node_modules/"));
        assert!(rendered.contains("safe-to-delete"));
        assert!(rendered.contains("will delete"));
        assert!(rendered.contains("needs approval"));
        assert!(rendered.contains("total · 2 item(s) · ~1.2 GB"));
        assert!(
            !rendered.contains('?'),
            "the panel must not embed a prompt: {rendered}"
        );
        assert!(!rendered.contains("[y/N]"), "review: {rendered}");
    }

    /// The same panel renders the post-approval fates, so a `--force` run
    /// never labels an item `needs approval` while deleting it.
    #[test]
    fn clean_review_carries_post_approval_fates() {
        let items = vec![CleanItem {
            rel_path: std::path::PathBuf::from("scratch.tmp"),
            is_dir: false,
            classification: Classification::Surfaced,
        }];
        let rows = clean_review_rows(&items, |_| "deleting");
        let rendered =
            format_clean_review(Path::new("/w/dash"), "main", &rows, None, 90, false).join("\n");
        assert!(rendered.contains("deleting"), "review: {rendered}");
        assert!(!rendered.contains("needs approval"), "review: {rendered}");
    }

    /// Item columns are aligned by display width, not by char count: a path
    /// of wide (CJK) characters must not push the label column right.
    #[test]
    fn clean_review_item_columns_align_by_display_width() {
        let items = vec![
            CleanItem {
                rel_path: std::path::PathBuf::from("node_modules"),
                is_dir: true,
                classification: Classification::Safe,
            },
            CleanItem {
                rel_path: std::path::PathBuf::from("工程目录"),
                is_dir: true,
                classification: Classification::Safe,
            },
        ];
        let rows = clean_review_rows(&items, |_| "will delete");
        let rendered = format_clean_review(Path::new("/w/dash"), "main", &rows, None, 90, false);
        let label_columns: Vec<usize> = rendered
            .iter()
            .filter(|line| line.contains("safe-to-delete"))
            .map(|line| {
                let idx = line.find("safe-to-delete").unwrap();
                line[..idx].width()
            })
            .collect();
        assert_eq!(label_columns.len(), 2, "rendered: {rendered:?}");
        assert_eq!(
            label_columns[0], label_columns[1],
            "wide-character paths must not shift the label column: {rendered:?}"
        );
    }

    /// Every wide-table column starts at the header's column, for every
    /// status. Colored labels carry ANSI escapes whose bytes must not be
    /// counted as padding — the regression this guards is a table that is
    /// ragged whenever color is on, which is the default on a TTY.
    #[test]
    fn table_columns_line_up_under_color_and_plain() {
        let rows = vec![
            ProjectTableRow {
                path: Path::new("/workspace/dashboard"),
                status: Status::Cleanable,
                size: Some("1.2 GB"),
                branch: Some("main"),
                changed: Some("2h ago"),
            },
            ProjectTableRow {
                path: Path::new("/workspace/design-system"),
                status: Status::Wip,
                size: Some("540 MB"),
                branch: Some("feat/tokens"),
                changed: Some("12m ago"),
            },
            ProjectTableRow {
                path: Path::new("/workspace/scratch"),
                status: Status::NoGit,
                size: None,
                branch: None,
                changed: None,
            },
        ];
        let plain = format_project_table(&rows, 1, Some("1.2 GB"), 100, false);
        let colored = format_project_table(&rows, 1, Some("1.2 GB"), 100, true);
        assert!(
            colored.join("\n").contains("\x1b["),
            "colored table should carry escapes"
        );
        let stripped: Vec<String> = colored.iter().map(|l| strip_ansi(l)).collect();
        assert_eq!(
            stripped, plain,
            "color must not change the visible layout by a single column"
        );

        let status_col = plain[0].find("STATUS").unwrap();
        for (i, row) in rows.iter().enumerate() {
            let line = &plain[i + 1];
            assert!(
                line[status_col..].starts_with(row.status.label()),
                "row {i} status column misaligned: {line:?}"
            );
        }
    }

    /// The PROJECT column keeps enough path to tell two same-named projects
    /// apart — a leaf name alone renders them as identical rows.
    #[test]
    fn table_project_column_disambiguates_same_leaf_names() {
        let rows = vec![
            ProjectTableRow {
                path: Path::new("/work/api"),
                status: Status::Cleanable,
                size: None,
                branch: None,
                changed: None,
            },
            ProjectTableRow {
                path: Path::new("/oss/api"),
                status: Status::Cleanable,
                size: None,
                branch: None,
                changed: None,
            },
        ];
        let rendered = format_project_table(&rows, 2, None, 100, false);
        assert!(rendered[1].contains("/work/api"), "{:?}", rendered[1]);
        assert!(rendered[2].contains("/oss/api"), "{:?}", rendered[2]);
        assert_ne!(rendered[1], rendered[2]);
    }

    /// Over-long project paths keep their tail (the distinguishing part) and
    /// lose the head to a leading ellipsis.
    #[test]
    fn table_project_column_truncates_from_the_left() {
        let rows = vec![ProjectTableRow {
            path: Path::new("/very/deeply/nested/workspace/tree/for/the/team/dashboard"),
            status: Status::Cleanable,
            size: None,
            branch: None,
            changed: None,
        }];
        let rendered = format_project_table(&rows, 1, None, 84, false);
        assert!(rendered[1].contains('…'), "{:?}", rendered[1]);
        assert!(rendered[1].contains("dashboard"), "{:?}", rendered[1]);
    }

    /// The success panel reports the size it is handed — the caller measures
    /// the approved items — and degrades to a count-only line without one.
    #[test]
    fn clean_success_reports_reclaimed_size_when_known() {
        let with_size =
            format_clean_success(Path::new("/w/dashboard"), 2, Some("1.2 GB"), false).join("\n");
        assert!(with_size.contains("~1.2 GB"), "{with_size}");
        assert!(with_size.contains("removed 2 item(s)"), "{with_size}");
        assert!(with_size.contains("dashboard"), "{with_size}");
        assert!(!with_size.contains("\x1b["), "{with_size}");

        let without = format_clean_success(Path::new("/w/dashboard"), 2, None, false).join("\n");
        assert!(without.contains("✓ removed 2 item(s)"), "{without}");
        assert!(!without.contains("reclaimed"), "{without}");
    }

    #[test]
    fn blocked_project_renders_refusal_and_next_action() {
        let rendered = format_blocked_project(
            Path::new("/workspace/design-system"),
            Status::Wip,
            "feat/tokens · uncommitted changes · remote ✓",
            &[" M src/tokens/color.ts".to_string()],
            Some("540 MB"),
            false,
        )
        .join("\n");
        assert!(rendered.contains("refusing to clean"));
        assert!(rendered.contains("uncommitted work in progress"));
        assert!(rendered.contains("M src/tokens/color.ts"));
        assert!(rendered.contains("would become reclaimable"));
    }

    #[test]
    fn terminal_ui_disabled_for_non_tty_streams() {
        assert!(!terminal_ui_enabled(false));
    }

    /// `TERM=dumb` degrades to the plain presentation even on a real TTY,
    /// and a capable (or unset) `TERM` on a TTY keeps the rich one. The gate
    /// takes `TERM` as an argument so this needs no environment mutation.
    #[test]
    fn terminal_ui_gate_honors_term_and_tty() {
        let dumb = std::ffi::OsString::from("dumb");
        let capable = std::ffi::OsString::from("xterm-256color");
        assert!(!terminal_ui_enabled_with(true, Some(&dumb)));
        assert!(terminal_ui_enabled_with(true, Some(&capable)));
        assert!(terminal_ui_enabled_with(true, None));
        // A non-TTY stream is plain regardless of TERM: piped output must
        // stay line-oriented for scripts.
        assert!(!terminal_ui_enabled_with(false, Some(&capable)));
        assert!(!terminal_ui_enabled_with(false, None));
    }

    /// The color gate is per-stream but shares the `NO_COLOR`/`CLICOLOR`
    /// conventions: a non-TTY stream never emits color.
    #[test]
    fn color_gate_is_off_for_non_tty_streams() {
        assert!(!color_enabled_for(false));
    }

    /// Every rich renderer degrades to plain text with `emit_colors = false`
    /// — the state a piped stream, `NO_COLOR`, or `TERM=dumb` lands in.
    #[test]
    fn rich_renderers_emit_no_escapes_when_colors_are_off() {
        let table_rows = vec![ProjectTableRow {
            path: Path::new("/workspace/dashboard"),
            status: Status::Cleanable,
            size: Some("1.2 GB"),
            branch: Some("main"),
            changed: Some("2h ago"),
        }];
        let items = vec![CleanItem {
            rel_path: std::path::PathBuf::from("node_modules"),
            is_dir: true,
            classification: Classification::Safe,
        }];
        let review_rows = clean_review_rows(&items, |_| "will delete");
        let rendered = [
            format_project_table(&table_rows, 1, Some("1.2 GB"), 100, false),
            format_project_table(&table_rows, 1, Some("1.2 GB"), 40, false),
            format_clean_review(Path::new("/w/dash"), "main", &review_rows, None, 90, false),
            format_blocked_project(
                Path::new("/w/dash"),
                Status::Wip,
                "main · uncommitted changes",
                &[" M src/a.ts".to_string()],
                Some("540 MB"),
                false,
            ),
            format_clean_success(Path::new("/w/dash"), 1, Some("1.2 GB"), false),
        ]
        .concat()
        .join("\n");
        assert!(!rendered.contains("\x1b["), "plain rendering: {rendered:?}");
    }

    /// Strip ANSI SGR sequences so a colored rendering can be compared
    /// against the plain one column for column.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for next in chars.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
