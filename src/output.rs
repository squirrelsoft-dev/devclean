//! Output formatting for offcut: colored, sorted, gated on TTY.
//!
//! Owns everything about how each line of output reads — labels, ranks,
//! color coding per status, the cleanable indicator, and the TTY-vs-piped
//! color gate. Colors appear only on a terminal that can render them —
//! a TTY that is not `TERM=dumb`; piped or redirected stdout gets plain
//! text, and setting `NO_COLOR` (any value) or `CLICOLOR=0` disables color
//! even on a capable TTY.
//!
//! Each formatted row follows the shape `[rank] path — label (reason)`,
//! with the reason omitted when it would merely repeat the label
//! (cleanable/clean rows). The label is color-coded per status so the
//! most-needs-attention rows stand out (dirty statuses in warm tones,
//! cleanable/clean in cool tones); the bold-cyan label is the cleanable
//! indicator, marking which projects are subjects of the cleaning flow.
//!
//! The `emit_colors` flag lets the formatting functions be unit-tested
//! without needing a real TTY — tests pass `Some(true)` to capture color
//! output and `Some(false)` to verify plain degradation.

use owo_colors::{OwoColorize, Style as OwoStyle};
use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::classify::Status;
use crate::clean::{Classification, CleanItem};
use crate::interactive::{blocked_reason, status_reason};

/// Process-wide gate set once in `main` when `--json` is active. Under JSON
/// mode the rich terminal UI, color, and live progress are all suppressed so
/// stdout carries exactly one JSON document — even when stdout is a TTY.
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Enable JSON mode for the rest of the process. Called once from `main`
/// after clap parses.
pub fn set_json_mode(enabled: bool) {
    JSON_MODE.store(enabled, Ordering::SeqCst);
}

/// Whether JSON mode is active.
pub fn json_mode() -> bool {
    JSON_MODE.load(Ordering::SeqCst)
}

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
    !json_mode() && terminal_ui_enabled(is_tty())
}

/// Whether stderr should receive the rich interactive prompt presentation.
pub fn stderr_terminal_ui_enabled() -> bool {
    !json_mode() && terminal_ui_enabled(stderr_is_tty())
}

/// Current terminal width, used by the panel and table renderers.
///
/// The terminal-size → `COLUMNS` → 80 fallback chain has exactly one
/// implementation, in `progress`; this is the presentation-side name for it so
/// the two cannot drift apart.
pub fn terminal_width() -> usize {
    crate::progress::terminal_width()
}

/// The noun for `count` projects, so no rendered line reads "1 projects".
pub fn projects_word(count: usize) -> &'static str {
    if count == 1 { "project" } else { "projects" }
}

/// Runtime color gate: colored only on a terminal that can render escapes,
/// and only when neither `NO_COLOR` (any value, per the no-color.org
/// convention) nor `CLICOLOR=0` asks for plain output.
fn color_enabled() -> bool {
    color_enabled_for(is_tty())
}

/// The color gate for one stream: the `NO_COLOR`/`CLICOLOR` conventions
/// apply to every stream; only the TTY test differs per stream.
fn color_enabled_for(stream_is_tty: bool) -> bool {
    color_enabled_with(
        stream_is_tty,
        std::env::var_os("TERM").as_deref(),
        plain_requested(),
    )
}

/// Whether the environment asks for plain output outright: `NO_COLOR` with any
/// value (per the no-color.org convention) or `CLICOLOR=0`.
fn plain_requested() -> bool {
    std::env::var_os("NO_COLOR").is_some() || std::env::var_os("CLICOLOR").is_some_and(|v| v == "0")
}

/// The gate itself, with every input passed in so it is testable without a real
/// pty and without mutating the process environment (which would race every
/// other test in the binary — and leave the gate's verdict at the mercy of
/// whatever `NO_COLOR` the test runner happens to export).
///
/// Whether escapes can be rendered at all is `terminal_ui_enabled_with`'s
/// question, asked once: a `TERM=dumb` stream renders neither the rich panels
/// nor SGR sequences, so a gate that only tested `is_tty` would print escape
/// codes as literal garbage into exactly the terminals the rich UI already
/// steps aside for (Emacs `M-x shell` is a pty with `TERM=dumb`). The
/// `NO_COLOR`/`CLICOLOR` conventions then subtract color from terminals that
/// could otherwise render it.
fn color_enabled_with(stream_is_tty: bool, term: Option<&OsStr>, plain_requested: bool) -> bool {
    !plain_requested && terminal_ui_enabled_with(stream_is_tty, term)
}

/// Whether stdout should emit ANSI color under the current environment.
pub fn stdout_color_enabled() -> bool {
    !json_mode() && color_enabled()
}

/// Whether stderr should emit ANSI color under the current environment.
/// Interactive prompts and their review panel go to stderr, so they need
/// their own gate rather than borrowing stdout's redirection state.
pub fn stderr_color_enabled() -> bool {
    !json_mode() && color_enabled_for(stderr_is_tty())
}

/// Color `text` with `style`, gated by `emit_colors`.
///
/// When `emit_colors` is `None`, uses the runtime gate (`color_enabled`: a
/// terminal that can render escapes — a TTY that is not `TERM=dumb` — with
/// neither `NO_COLOR` nor `CLICOLOR=0` asking for plain output). When
/// `Some(false)` or `Some(true)` uses that directly — the way unit tests verify
/// color behavior without needing a real TTY.
///
/// The plain fallback path reaches here with `None`
/// (`format_project_row`/`format_summary`), so this gate is what keeps a
/// terminal that gets the plain layout from getting escape sequences with it.
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
/// the most-needs-attention rows stand out at a glance; the bold-cyan
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
/// the rest. Cleanable and clean use cool tones — bold cyan for the
/// ready-to-clean case so the indicator stands out from the green that marks
/// an already-settled project.
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
    let counted = format!("✓ {} {}", rows.len(), projects_word(rows.len()));
    let summary = match total_reclaimable {
        Some(size) if cleanable_count > 0 => vec![
            counted,
            format!("~{size} reclaimable across {cleanable_count} cleanable"),
        ],
        _ => vec![counted, format!("{cleanable_count} cleanable")],
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
            let (marker, label_width) = prefix_budget("● ", width);
            out.push(format!(
                "{}{}",
                color_for_stream(&marker, status_style(row.status), emit_colors),
                truncate_path_cols(&project_label(row.path), label_width)
            ));
            let (indent, detail_width) = prefix_budget("  ", width);
            let status_label = truncate_cols(row.status.label(), detail_width);
            let detail = [
                (
                    status_label.width(),
                    colored_status(&status_label, row.status, emit_colors),
                ),
                cell(&truncate_cols(
                    &format!("reclaim {}", row.size.unwrap_or("-")),
                    detail_width,
                )),
                cell(&truncate_cols(
                    &format!("branch {}", row.branch.unwrap_or("-")),
                    detail_width,
                )),
                cell(&truncate_cols(
                    &format!("changed {}", row.changed.unwrap_or("-")),
                    detail_width,
                )),
            ];
            for line in pack_columns(&detail, detail_width, " · ") {
                out.push(format!("{indent}{line}"));
            }
        }
    }

    out.push(String::new());
    let summary: Vec<(usize, String)> = summary
        .iter()
        .map(|segment| {
            (
                segment.width(),
                color_for_stream(segment, OwoStyle::new().green().bold(), emit_colors),
            )
        })
        .collect();
    out.extend(pack_columns(&summary, width, " · "));
    let hint = [
        (
            "run offcut clean to reclaim all,".width(),
            format!(
                "run {} to reclaim all,",
                color_for_stream("offcut clean", OwoStyle::new().bold(), emit_colors)
            ),
        ),
        (
            "or offcut clean <project> to inspect one".width(),
            format!(
                "or {} to inspect one",
                color_for_stream(
                    "offcut clean <project>",
                    OwoStyle::new().bold(),
                    emit_colors
                )
            ),
        ),
    ];
    out.extend(pack_columns(&hint, width, " "));
    out.push(String::new());
    let legend: Vec<(usize, String)> = [
        (Status::Cleanable, "cleanable - safe to remove"),
        (Status::Clean, "clean - nothing to trim"),
        (Status::Wip, "wip - uncommitted work"),
        (Status::NoRemote, "no-remote/unpushed - not pushed"),
        (Status::NoGit, "no-git - not a repo"),
    ]
    .iter()
    .map(|(status, text)| {
        (
            "● ".width() + text.width(),
            format!(
                "{} {text}",
                color_for_stream("●", status_style(*status), emit_colors)
            ),
        )
    })
    .collect();
    out.extend(pack_columns(&legend, width, "  "));
    out
}

/// One packable cell whose rendering carries no styling, so its display width
/// is its own.
fn cell(text: &str) -> (usize, String) {
    (text.width(), text.to_string())
}

/// Pack `entries` into as few `sep`-joined lines as fit `width`.
///
/// Each entry carries its own display width because a styled entry's string
/// length includes ANSI bytes that occupy no columns — measuring the rendered
/// string would over-count and wrap early, and measuring nothing at all is
/// what makes a legend soft-wrap mid-entry on an ordinary 80-column terminal.
/// An entry wider than `width` still gets its own line rather than being
/// dropped or truncated.
fn pack_columns(entries: &[(usize, String)], width: usize, sep: &str) -> Vec<String> {
    let sep_width = sep.width();
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for (entry_width, rendered) in entries {
        if line.is_empty() {
            line.push_str(rendered);
            line_width = *entry_width;
        } else if line_width + sep_width + entry_width > width {
            lines.push(std::mem::take(&mut line));
            line.push_str(rendered);
            line_width = *entry_width;
        } else {
            line.push_str(sep);
            line.push_str(rendered);
            line_width += sep_width + entry_width;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Column widths for the clean-review item list: the widest classification
/// label (`safe-to-delete`) and the widest fate (`needs approval`).
const LABEL_W: usize = 14;
const FATE_W: usize = 14;

/// Narrowest item column the three-column review row stays readable in, and
/// the terminal width that implies once the prefix, gaps, label, and fate are
/// accounted for. Below it the row degrades to a stacked layout — the same
/// treatment the workspace table gives a terminal too narrow for its columns.
const REVIEW_MIN_ITEM_W: usize = 12;
const REVIEW_COLUMNAR_MIN_W: usize = 4 + REVIEW_MIN_ITEM_W + 1 + LABEL_W + 1 + FATE_W;

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
    let mut out = panel_head(path, Status::Cleanable, branch_line, width, emit_colors);
    out.push(dim(&truncate_cols("GITIGNORED REVIEW", width), emit_colors));
    // " ▸ " prefix (4) + item + gap (1) + label + gap (1) + fate.
    let columnar = width >= REVIEW_COLUMNAR_MIN_W;
    let item_width = width.saturating_sub(4 + 1 + LABEL_W + 1 + FATE_W);
    for row in rows {
        let item = format!(
            "{}{}",
            row.rel_path.display(),
            if row.is_dir { "/" } else { "" }
        );
        if columnar {
            out.push(format!(
                "  ▸ {} {} {}",
                pad_path(&item, item_width),
                styled_padded(row.label, LABEL_W, OwoStyle::new().dimmed(), emit_colors),
                dim(row.fate, emit_colors)
            ));
            continue;
        }
        // Too narrow for three columns: stack the classification and the fate
        // under the path instead of letting every row soft-wrap, which is the
        // one thing the aligned panel exists to prevent.
        let (bullet, item_budget) = prefix_budget("  ▸ ", width);
        out.push(format!(
            "{bullet}{}",
            truncate_path_cols(&item, item_budget)
        ));
        let (indent, body_width) = prefix_budget("    ", width);
        let label = truncate_cols(row.label, body_width);
        let fate = truncate_cols(row.fate, body_width);
        let detail = [
            (label.width(), dim(&label, emit_colors)),
            (fate.width(), dim(&fate, emit_colors)),
        ];
        for line in pack_columns(&detail, body_width, " · ") {
            out.push(format!("{indent}{line}"));
        }
    }
    out.extend(wrap_plain(
        &format!(
            "total · {} item(s){}",
            rows.len(),
            total_size.map(|s| format!(" · ~{s}")).unwrap_or_default()
        ),
        width,
    ));
    out
}

/// The three lines every project panel opens with: the header, the status, and
/// the branch state, followed by a blank separator. Shared by the review, the
/// blocked state, and the nothing-to-reclaim state so one panel cannot drift
/// out of the width budget the others respect.
fn panel_head(
    path: &Path,
    status: Status,
    branch_line: &str,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let (status_key, status_width) = prefix_budget("status   ", width);
    let (branch_key, branch_width) = prefix_budget("branch   ", width);
    vec![
        format_review_header(path, width, emit_colors),
        format!(
            "{status_key}{}",
            color_for_stream(
                &truncate_cols(status.label(), status_width),
                status_style(status),
                emit_colors
            )
        ),
        format!("{branch_key}{}", truncate_cols(branch_line, branch_width)),
        String::new(),
    ]
}

/// Split `width` between a fixed prefix (a key, a bullet, an indent) and the
/// body that follows it: the prefix is clipped first, then the body gets what
/// is left. A prefix emitted at its full length is how a rendered line
/// overruns a terminal narrower than the prefix itself.
fn prefix_budget(prefix: &str, width: usize) -> (String, usize) {
    let clipped = clip_cols(prefix, width);
    let budget = width.saturating_sub(clipped.width());
    (clipped, budget)
}

/// Truncate to `width` display columns with no ellipsis. Used for the fixed
/// parts of a line, where an ellipsis would read as lost content rather than
/// as the layout giving way.
fn clip_cols(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut cols = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if cols + ch_width > width {
            break;
        }
        cols += ch_width;
        out.push(ch);
    }
    out
}

/// Split `text` into word cells no wider than `width`, so packing them can
/// never produce a line that overruns the terminal.
fn word_cells(text: &str, width: usize) -> Vec<(usize, String)> {
    text.split_whitespace()
        .map(|word| cell(&truncate_cols(word, width)))
        .collect()
}

/// Word-wrap `text` into lines of at most `width` display columns.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    pack_columns(&word_cells(text, width), width, " ")
}

/// Word-wrap `text` and style each produced line. Styling per line rather than
/// per paragraph keeps the escape sequences out of the width measurement.
fn wrap_styled(text: &str, width: usize, style: OwoStyle, emit_colors: bool) -> Vec<String> {
    wrap_plain(text, width)
        .iter()
        .map(|line| color_for_stream(line, style, emit_colors))
        .collect()
}

/// Format the review panel's header — `⟩ analyzing <name> · <path>` — inside
/// `width` display columns.
///
/// It is the one panel line carrying both the project name and its full path,
/// so it is also the one that soft-wraps first. The name identifies the panel
/// and takes its columns first; the path gets what is left, truncated from the
/// left so the distinguishing tail survives. With no columns left for a path,
/// the separator goes with it rather than dangling at the end of the line.
fn format_review_header(path: &Path, width: usize, emit_colors: bool) -> String {
    const PREFIX: &str = "⟩ analyzing ";
    const SEP: &str = " · ";
    let prefix = truncate_cols(PREFIX, width);
    let budget = width.saturating_sub(prefix.width());
    let name = truncate_path_cols(&project_name(path), budget);
    let path_budget = budget.saturating_sub(name.width() + SEP.width());
    let mut line = format!(
        "{prefix}{}",
        color_for_stream(&name, OwoStyle::new().bold(), emit_colors)
    );
    if path_budget > 0 {
        line.push_str(SEP);
        line.push_str(&dim(
            &truncate_path_cols(&path.display().to_string(), path_budget),
            emit_colors,
        ));
    }
    line
}

/// Format the blocked clean state for a project whose tree blocks cleaning.
///
/// Only the statuses that actually block belong here (see `blocks_cleaning`):
/// telling the user to commit and push a project that is already committed and
/// pushed is a refusal it cannot act on. An already-clean project gets
/// `format_nothing_to_reclaim` instead.
///
/// `committed` says whether the project has any commit, the one fact the status
/// does not carry (see `interactive::blocked_reason`). The refusal is built from
/// it so it cannot contradict the branch state printed two lines above it.
///
/// Every line is wrapped or truncated to `width`, prose included: this panel
/// carries the longest sentences Offcut prints, and an unwrapped one soft-wraps
/// on an ordinary 80-column terminal.
pub fn format_blocked_project(
    path: &Path,
    status: Status,
    committed: bool,
    branch_line: &str,
    details: &[String],
    possible_reclaim: Option<&str>,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let mut out = panel_head(path, status, branch_line, width, emit_colors);
    out.extend(wrap_styled(
        &format!(
            "✗ refusing to clean - {}",
            blocked_reason(status, committed)
        ),
        width,
        status_style(status).bold(),
        emit_colors,
    ));
    out.extend(wrap_plain(
        "offcut only cleans projects with a clean, pushed tree, so nothing in progress is ever lost.",
        width,
    ));
    let (indent, detail_width) = prefix_budget("   ", width);
    for detail in details {
        out.push(format!(
            "{indent}{}",
            dim(&truncate_cols(detail, detail_width), emit_colors)
        ));
    }
    // The command carries the project path, so it is the one segment that can
    // outgrow the terminal on its own. The verb is what makes the hint
    // runnable, so it keeps its columns and the path spends what is left,
    // truncated from the left so the leaf survives.
    let (verb, path_budget) = prefix_budget("offcut clean ", width);
    let command = format!(
        "{verb}{}",
        truncate_path_cols(&path.display().to_string(), path_budget)
    );
    let mut hint = word_cells("→ commit, push, or initialize as needed, then run", width);
    hint.push((
        command.width(),
        color_for_stream(&command, OwoStyle::new().bold(), emit_colors),
    ));
    hint.extend(word_cells("again", width));
    out.extend(pack_columns(&hint, width, " "));
    if let Some(size) = possible_reclaim {
        out.extend(
            wrap_styled(
                &format!("~{size} would become reclaimable once the project is cleanable."),
                detail_width,
                OwoStyle::new().dimmed(),
                emit_colors,
            )
            .into_iter()
            .map(|line| format!("{indent}{line}")),
        );
    }
    out
}

/// Format the already-tidy state: a targeted run against a `Clean` project.
///
/// There is nothing to refuse and nothing to ask for, so the panel reports the
/// absence of work rather than a blocked action.
///
/// `Clean` does not mean the project has no build output — it means it has no
/// *unprotected* untracked junk (see `classify::classify`), so a project whose
/// `node_modules` is protected by `.offcutignore` lands here with that output
/// still on disk. The copy says what the state actually guarantees; claiming
/// nothing was found would deny the very output offcut is preserving.
pub fn format_nothing_to_reclaim(
    path: &Path,
    branch_line: &str,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let mut out = panel_head(path, Status::Clean, branch_line, width, emit_colors);
    out.extend(wrap_styled(
        &format!(
            "✓ nothing to reclaim - {} is already tidy",
            project_name(path)
        ),
        width,
        OwoStyle::new().green().bold(),
        emit_colors,
    ));
    out.extend(wrap_plain(
        "committed and pushed, with no unprotected gitignored paths left - anything still here is protected by .offcutignore.",
        width,
    ));
    out
}

/// Whether `status` is a tree state that blocks cleaning and needs the user to
/// act before a rerun can do anything.
///
/// `Clean` is deliberately not blocked — it is the already-tidy outcome — and
/// `Cleanable` is the state the flow acts on.
pub fn blocks_cleaning(status: Status) -> bool {
    match status {
        Status::NoGit | Status::NoRemote | Status::Unpushed | Status::Wip => true,
        Status::Cleanable | Status::Clean => false,
    }
}

/// Format the post-clean success state.
///
/// Both lines are prose, so both wrap to `width` rather than truncating: this
/// panel closes a destructive run, and a soft-wrapped reassurance about what
/// was *not* touched is the last line that should be allowed to overrun.
pub fn format_clean_success(
    path: &Path,
    deleted_count: usize,
    reclaimed: Option<&str>,
    width: usize,
    emit_colors: bool,
) -> Vec<String> {
    let size = reclaimed
        .map(|s| format!("reclaimed ~{s} - "))
        .unwrap_or_default();
    let mut out = wrap_styled(
        &format!(
            "✓ {size}removed {deleted_count} item(s) from {}",
            project_name(path)
        ),
        width,
        OwoStyle::new().green().bold(),
        emit_colors,
    );
    out.extend(wrap_plain(
        "gitignored paths only - tracked files untouched.",
        width,
    ));
    out
}

/// Format one `<lead><path>` status line inside `width` display columns.
///
/// The lead names what is happening and keeps its columns; the path spends
/// what is left, truncated from the left so the leaf survives. Every rendered
/// line carrying a project path goes through here — a path emitted at full
/// length behind a fixed prefix is the one shape that soft-wraps on an
/// ordinary terminal no matter how carefully the panels around it are budgeted.
pub fn format_path_line(lead: &str, path: &Path, width: usize) -> String {
    let (lead, budget) = prefix_budget(lead, width);
    format!(
        "{lead}{}",
        truncate_path_cols(&path.display().to_string(), budget)
    )
}

/// Word-wrap a standalone status line to `width` display columns. The
/// presentation-side name for the panels' own wrapping, so a line printed
/// outside a panel is bounded by the same rule as one printed inside it.
pub fn wrap_line(text: &str, width: usize) -> Vec<String> {
    wrap_plain(text, width)
}

fn colored_status(label: &str, status: Status, emit_colors: bool) -> String {
    color_for_stream(label, status_style(status), emit_colors)
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

    /// The widths every panel sweep runs. It reaches below the widest fixed
    /// prefix a panel prints (`"status   "`, 9 columns) because a prefix
    /// emitted at full length regardless of the terminal is exactly how the
    /// width invariant used to break — `COLUMNS=6` reaches it.
    const EXTREME_WIDTHS: [usize; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 40, 80];

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
    /// (bold cyan) is the cleanable indicator, not a repeated word.
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

    /// Each clean status gets a cool color: bold cyan marks the ready-to-clean
    /// row, plain green the already-settled one.
    ///
    /// The expected sequences are spelled out here rather than derived from
    /// `status_style`, and are matched exactly rather than as "some escape" —
    /// an any-escape assertion passes for every color, which is how the
    /// documented palette drifted away from the emitted one unnoticed.
    #[test]
    fn clean_statuses_get_cool_colors() {
        let cleanable = format_project_row(
            Path::new("/tmp/project"),
            Status::Cleanable,
            emit_true(),
            None,
        );
        let expect_cleanable = format!("{}", "cleanable".style(OwoStyle::new().cyan().bold()));
        assert!(
            cleanable.contains(&expect_cleanable),
            "cleanable is cyan+bold: {cleanable:?}"
        );

        let clean = format_project_row(Path::new("/tmp/project"), Status::Clean, emit_true(), None);
        let expect_clean = format!("{}", "clean".style(OwoStyle::new().green()));
        assert!(clean.contains(&expect_clean), "clean is green: {clean:?}");
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

    /// The review header carries both the project name and its full path, so
    /// a long path or a narrow terminal used to soft-wrap it onto a second
    /// line while every other panel line stayed inside the width.
    #[test]
    fn clean_review_header_fits_the_terminal_width() {
        let items = vec![CleanItem {
            rel_path: std::path::PathBuf::from("node_modules"),
            is_dir: true,
            classification: Classification::Safe,
        }];
        let rows = clean_review_rows(&items, |_| "will delete");
        let path = Path::new(
            "/var/folders/s0/zl64g7m92b7bf0d72vc0cskw0000gn/T/offcut-demo/projects/payments-api",
        );
        for width in [10, 20, 40, 56, 60, 80, 120] {
            for emit_colors in [false, true] {
                let header = strip_ansi(
                    &format_clean_review(
                        path,
                        "main · clean tree · remote ✓ pushed",
                        &rows,
                        Some("2.34 MB"),
                        width,
                        emit_colors,
                    )[0],
                );
                assert!(
                    header.width() <= width,
                    "width {width} (colors {emit_colors}): header of {} columns: {header:?}",
                    header.width()
                );
            }
        }
    }

    /// The header is not the only line that can overrun: the branch state, the
    /// item rows, and the total each carry caller-supplied text. Below the
    /// width the three-column row needs, the rows stack rather than wrap.
    #[test]
    fn every_clean_review_line_fits_the_terminal_width() {
        let items = vec![
            CleanItem {
                rel_path: std::path::PathBuf::from("node_modules"),
                is_dir: true,
                classification: Classification::Safe,
            },
            CleanItem {
                rel_path: std::path::PathBuf::from(
                    "packages/design-system/.turbo/cache/build-output",
                ),
                is_dir: true,
                classification: Classification::Surfaced,
            },
        ];
        let rows = clean_review_rows(&items, |_| "needs approval");
        let path = Path::new(
            "/var/folders/s0/zl64g7m92b7bf0d72vc0cskw0000gn/T/offcut-demo/projects/payments-api",
        );
        for width in EXTREME_WIDTHS.iter().copied().chain([45, 46, 56, 120]) {
            for emit_colors in [false, true] {
                for line in format_clean_review(
                    path,
                    "feature/some-long-branch-name · clean tree · remote ✓ pushed",
                    &rows,
                    Some("2.34 MB"),
                    width,
                    emit_colors,
                ) {
                    let visible = strip_ansi(&line);
                    assert!(
                        visible.width() <= width,
                        "width {width} (colors {emit_colors}): line of {} columns: {visible:?}",
                        visible.width()
                    );
                }
            }
        }
    }

    /// Below the columnar minimum, each item's classification and fate move to
    /// their own line under the path instead of being squeezed into columns
    /// that no longer fit — the same degradation the workspace table makes.
    #[test]
    fn clean_review_stacks_item_rows_on_a_narrow_terminal() {
        let items = vec![CleanItem {
            rel_path: std::path::PathBuf::from("node_modules"),
            is_dir: true,
            classification: Classification::Safe,
        }];
        let rows = clean_review_rows(&items, |_| "will delete");
        let narrow = format_clean_review(Path::new("/w/dash"), "main", &rows, None, 40, false);
        let item_line = narrow
            .iter()
            .find(|line| line.contains("node_modules"))
            .expect("item row rendered");
        assert!(
            !item_line.contains("safe-to-delete"),
            "narrow rows stack: {narrow:?}"
        );
        let detail = narrow
            .iter()
            .find(|line| line.contains("safe-to-delete"))
            .expect("classification rendered");
        assert!(detail.contains("will delete"), "detail: {detail:?}");

        // Above the minimum the three-column row is back.
        const { assert!(REVIEW_COLUMNAR_MIN_W > 40) };
        let wide = format_clean_review(Path::new("/w/dash"), "main", &rows, None, 60, false);
        assert!(
            wide.iter()
                .any(|line| line.contains("node_modules") && line.contains("safe-to-delete")),
            "columnar rows: {wide:?}"
        );
    }

    /// Truncating the header must not cost it the project's name — that is
    /// what tells the user which project the panel is about.
    #[test]
    fn clean_review_header_keeps_the_project_name() {
        let rows = Vec::new();
        let path = Path::new("/a/very/long/workspace/path/that/eats/the/line/payments-api");
        let header = strip_ansi(&format_clean_review(path, "main", &rows, None, 56, false)[0]);
        assert!(
            header.starts_with("⟩ analyzing payments-api · "),
            "header: {header:?}"
        );
        assert!(header.ends_with("/payments-api"), "header: {header:?}");
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

    /// No rendered line overruns the terminal, at any width the table is
    /// asked for. The legend and the next-step hint are the long unmeasured
    /// shapes: unpacked, the legend alone is ~139 columns and soft-wraps
    /// mid-entry on every ordinary 80–100 column terminal.
    #[test]
    fn every_table_line_fits_the_terminal_width() {
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
        for width in [40, 50, 80, 84, 100, 142] {
            for emit_colors in [false, true] {
                for line in format_project_table(&rows, 1, Some("1.2 GB"), width, emit_colors) {
                    let visible = strip_ansi(&line);
                    assert!(
                        visible.width() <= width,
                        "width {width} (colors {emit_colors}): line of {} columns: {visible:?}",
                        visible.width()
                    );
                }
            }
        }
    }

    /// Every project row fits at *any* width, down to a single column. The
    /// stacked rows carry fixed prefixes (the status dot, the detail indent),
    /// and a prefix emitted at full length is what overruns a terminal
    /// narrower than the prefix itself.
    ///
    /// Only the rows are swept: the footer, hint, and legend below the blank
    /// separator are packed by `pack_columns`, which deliberately gives an
    /// over-wide entry its own line rather than dropping it.
    #[test]
    fn every_table_row_fits_even_the_narrowest_terminal() {
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
                size: None,
                branch: None,
                changed: None,
            },
        ];
        for width in EXTREME_WIDTHS {
            for emit_colors in [false, true] {
                let rendered = format_project_table(&rows, 1, Some("1.2 GB"), width, emit_colors);
                let body = rendered
                    .iter()
                    .position(|line| line.is_empty())
                    .unwrap_or(rendered.len());
                assert!(body > 0, "width {width}: table rendered no rows");
                for line in &rendered[..body] {
                    let visible = strip_ansi(line);
                    assert!(
                        visible.width() <= width,
                        "width {width} (colors {emit_colors}): row of {} columns: {visible:?}",
                        visible.width()
                    );
                }
            }
        }
    }

    /// The legend keeps every entry intact while packing to the width — an
    /// entry is never split across two lines.
    #[test]
    fn legend_packs_whole_entries_across_lines() {
        let rows = vec![ProjectTableRow {
            path: Path::new("/workspace/dashboard"),
            status: Status::Cleanable,
            size: Some("1.2 GB"),
            branch: Some("main"),
            changed: Some("2h ago"),
        }];
        let rendered = format_project_table(&rows, 1, Some("1.2 GB"), 80, false).join("\n");
        for entry in [
            "cleanable - safe to remove",
            "clean - nothing to trim",
            "wip - uncommitted work",
            "no-remote/unpushed - not pushed",
            "no-git - not a repo",
        ] {
            assert!(
                rendered.contains(entry),
                "legend lost {entry:?}: {rendered}"
            );
        }
        // Wide enough for one line, narrow enough to need more than one.
        let one_line = format_project_table(&rows, 1, Some("1.2 GB"), 200, false);
        let packed = format_project_table(&rows, 1, Some("1.2 GB"), 80, false);
        assert!(
            packed.len() > one_line.len(),
            "a narrower terminal must use more lines, not a wrapped one"
        );
    }

    /// An entry wider than the whole terminal still gets a line of its own
    /// rather than being dropped.
    #[test]
    fn pack_columns_keeps_oversized_entries() {
        let entries = [cell("a"), cell("an-extremely-long-single-entry"), cell("b")];
        let lines = pack_columns(&entries, 10, "  ");
        assert_eq!(
            lines,
            vec!["a", "an-extremely-long-single-entry", "b"],
            "lines: {lines:?}"
        );
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
            format_clean_success(Path::new("/w/dashboard"), 2, Some("1.2 GB"), 80, false)
                .join("\n");
        assert!(with_size.contains("~1.2 GB"), "{with_size}");
        assert!(with_size.contains("removed 2 item(s)"), "{with_size}");
        assert!(with_size.contains("dashboard"), "{with_size}");
        assert!(!with_size.contains("\x1b["), "{with_size}");

        let without =
            format_clean_success(Path::new("/w/dashboard"), 2, None, 80, false).join("\n");
        assert!(without.contains("✓ removed 2 item(s)"), "{without}");
        assert!(!without.contains("reclaimed"), "{without}");
    }

    #[test]
    fn blocked_project_renders_refusal_and_next_action() {
        let rendered = format_blocked_project(
            Path::new("/workspace/design-system"),
            Status::Wip,
            true,
            "feat/tokens · uncommitted changes · remote ✓",
            &[" M src/tokens/color.ts".to_string()],
            Some("540 MB"),
            100,
            false,
        )
        .join("\n");
        assert!(rendered.contains("refusing to clean"));
        assert!(rendered.contains("uncommitted work in progress"));
        assert!(rendered.contains("M src/tokens/color.ts"));
        assert!(rendered.contains("would become reclaimable"));
    }

    /// The refusal and the branch state sit two lines apart, so they must agree
    /// about whether the project has commits. `Unpushed` covers a repo that has
    /// never committed anything, and the panel's own header says so — a refusal
    /// built from the status alone would contradict the line above it.
    #[test]
    fn blocked_refusal_agrees_with_the_branch_state_about_commits() {
        let render = |committed: bool, branch: &str| {
            format_blocked_project(
                Path::new("/workspace/api"),
                Status::Unpushed,
                committed,
                branch,
                &[],
                None,
                100,
                false,
            )
            .join("\n")
        };

        let empty = render(false, "main · no commits yet · remote ✗ nothing pushed");
        assert!(empty.contains("no commits yet"), "{empty}");
        assert!(
            empty.contains("refusing to clean - nothing committed yet"),
            "{empty}"
        );
        assert!(!empty.contains("has unpushed commits"), "{empty}");

        let ahead = render(true, "main · local commits ahead · remote ✗ not pushed");
        assert!(
            ahead.contains("refusing to clean - has unpushed commits"),
            "{ahead}"
        );
        assert!(!ahead.contains("nothing committed"), "{ahead}");
    }

    /// The blocked panel carries the longest prose Offcut prints plus the full
    /// project path in its rerun hint. Unwrapped, both overran an ordinary
    /// 80-column terminal and soft-wrapped into the next line.
    #[test]
    fn blocked_panel_fits_the_terminal_width() {
        let path = Path::new(
            "/var/folders/s0/zl64g7m92b7bf0d72vc0cskw0000gn/T/offcut-demo/projects/design-system",
        );
        for width in EXTREME_WIDTHS {
            for emit_colors in [false, true] {
                for line in format_blocked_project(
                    path,
                    Status::Wip,
                    true,
                    "feat/tokens · uncommitted changes · remote ✓",
                    &[" M src/tokens/color-primitives-and-aliases.ts".to_string()],
                    Some("540 MB"),
                    width,
                    emit_colors,
                ) {
                    let visible = strip_ansi(&line);
                    assert!(
                        visible.width() <= width,
                        "width {width} (colors {emit_colors}): line of {} columns: {visible:?}",
                        visible.width()
                    );
                }
            }
        }
    }

    /// The rerun hint is only worth printing if it can be run. Truncating the
    /// whole command from the left ate `offcut clean ` first and left the
    /// sentence pointing at a bare path fragment — for any project path past
    /// 67 columns on an ordinary 80-column terminal.
    #[test]
    fn blocked_hint_keeps_the_command_runnable() {
        let path = Path::new(
            "/Users/someone/work/very/deeply/nested/monorepo/packages/design-system-tokens",
        );
        for width in [40, 60, 80, 100] {
            let rendered = format_blocked_project(
                path,
                Status::Wip,
                true,
                "feat/tokens · uncommitted changes · remote ✓",
                &[],
                None,
                width,
                false,
            )
            .join("\n");
            assert!(
                rendered.contains("offcut clean "),
                "width {width} lost the command verb: {rendered}"
            );
            assert!(
                rendered.contains("design-system-tokens"),
                "width {width} lost the project leaf: {rendered}"
            );
        }
    }

    /// Wrapping the blocked panel must not cost it the reason, the git detail,
    /// or the command that unblocks the project.
    #[test]
    fn blocked_panel_keeps_its_content_when_wrapped() {
        let rendered = format_blocked_project(
            Path::new("/workspace/design-system"),
            Status::Wip,
            true,
            "feat/tokens · uncommitted changes · remote ✓",
            &[" M src/tokens/color.ts".to_string()],
            Some("540 MB"),
            60,
            false,
        )
        .join("\n");
        assert!(rendered.contains("refusing to clean"), "{rendered}");
        assert!(rendered.contains("uncommitted work"), "{rendered}");
        assert!(rendered.contains("M src/tokens/color.ts"), "{rendered}");
        assert!(rendered.contains("offcut clean"), "{rendered}");
        assert!(rendered.contains("design-system"), "{rendered}");
    }

    /// An already-clean project is committed, pushed, and carries nothing
    /// offcut may delete: refusing to clean it and telling the user to commit
    /// and push is advice they cannot act on. It gets the nothing-to-reclaim
    /// state instead.
    #[test]
    fn nothing_to_reclaim_state_makes_no_refusal() {
        let rendered = format_nothing_to_reclaim(
            Path::new("/workspace/dashboard"),
            "main · clean tree · remote ✓ pushed",
            80,
            false,
        )
        .join("\n");
        assert!(rendered.contains("nothing to reclaim"), "{rendered}");
        assert!(rendered.contains("dashboard"), "{rendered}");
        assert!(rendered.contains(Status::Clean.label()), "{rendered}");
        assert!(!rendered.contains("refusing to clean"), "{rendered}");
        assert!(!rendered.contains("commit, push"), "{rendered}");

        for width in EXTREME_WIDTHS {
            for line in format_nothing_to_reclaim(
                Path::new("/workspace/dashboard"),
                "main · clean tree · remote ✓ pushed",
                width,
                true,
            ) {
                let visible = strip_ansi(&line);
                assert!(
                    visible.width() <= width,
                    "width {width}: line of {} columns: {visible:?}",
                    visible.width()
                );
            }
        }
    }

    /// The panel that closes a destructive run says what was removed and what
    /// was left alone — and stays inside the terminal at every width, including
    /// the ones narrower than its own reassurance sentence.
    #[test]
    fn clean_success_panel_fits_every_width() {
        let path = Path::new("/var/folders/s0/zl64g7m92b7bf0d72vc0cskw0000gn/T/demo/payments-api");
        let rendered = format_clean_success(path, 3, Some("1.2 GB"), 80, false).join("\n");
        assert!(rendered.contains("payments-api"), "{rendered}");
        assert!(rendered.contains("reclaimed ~1.2 GB"), "{rendered}");
        assert!(rendered.contains("tracked files untouched"), "{rendered}");

        for width in EXTREME_WIDTHS {
            for emit_colors in [false, true] {
                for reclaimed in [None, Some("1.2 GB")] {
                    for line in format_clean_success(path, 3, reclaimed, width, emit_colors) {
                        let visible = strip_ansi(&line);
                        assert!(
                            visible.width() <= width,
                            "width {width} (colors {emit_colors}): line of {} columns: {visible:?}",
                            visible.width()
                        );
                    }
                }
            }
        }
    }

    /// Every rendered line that carries a project path is bounded the same way,
    /// panel or not: the lead keeps its columns, the path spends what is left
    /// and keeps its leaf. A full path behind a fixed prefix is what soft-wraps
    /// an otherwise carefully budgeted screen.
    #[test]
    fn path_lines_fit_every_width() {
        let path = Path::new("/var/folders/s0/zl64g7m92b7bf0d72vc0cskw0000gn/T/demo/payments-api");
        assert!(
            format_path_line("⟩ cleaning ", path, 80).contains("payments-api"),
            "the leaf survives at a normal width"
        );
        for width in EXTREME_WIDTHS {
            for lead in ["⟩ cleaning ", "⟩ skipped by user · "] {
                let visible = format_path_line(lead, path, width);
                assert!(
                    visible.width() <= width,
                    "width {width}, lead {lead:?}: line of {} columns: {visible:?}",
                    visible.width()
                );
                for line in wrap_line("⟩ dry-run only - nothing deleted", width) {
                    assert!(
                        line.width() <= width,
                        "width {width}: wrapped line of {} columns: {line:?}",
                        line.width()
                    );
                }
            }
        }
    }

    /// `Clean` means no *unprotected* untracked junk, not an empty tree: a
    /// project whose `node_modules` is protected by `.offcutignore` classifies
    /// `Clean` with that output still on disk. The panel must not deny the
    /// existence of the output offcut is deliberately preserving.
    #[test]
    fn nothing_to_reclaim_state_does_not_deny_protected_output() {
        let rendered = format_nothing_to_reclaim(
            Path::new("/workspace/dashboard"),
            "main · clean tree · remote ✓ pushed",
            100,
            false,
        )
        .join(" ");
        assert!(
            !rendered.contains("was found"),
            "the state cannot claim nothing was found: {rendered}"
        );
        assert!(rendered.contains(".offcutignore"), "{rendered}");
        assert!(rendered.contains("protected"), "{rendered}");
    }

    /// Only a tree state the user can act on blocks cleaning. `Clean` is the
    /// already-tidy outcome and `Cleanable` is what the flow acts on, so
    /// neither may be routed to the refusal panel.
    #[test]
    fn only_actionable_tree_states_block_cleaning() {
        assert!(blocks_cleaning(Status::NoGit));
        assert!(blocks_cleaning(Status::NoRemote));
        assert!(blocks_cleaning(Status::Unpushed));
        assert!(blocks_cleaning(Status::Wip));
        assert!(!blocks_cleaning(Status::Cleanable));
        assert!(!blocks_cleaning(Status::Clean));
    }

    /// A one-project run must not read "1 projects".
    #[test]
    fn table_summary_counts_one_project_in_the_singular() {
        let rows = vec![ProjectTableRow {
            path: Path::new("/workspace/dashboard"),
            status: Status::Cleanable,
            size: Some("1.2 GB"),
            branch: Some("main"),
            changed: Some("2h ago"),
        }];
        let one = format_project_table(&rows, 1, Some("1.2 GB"), 100, false).join("\n");
        assert!(one.contains("✓ 1 project"), "{one}");
        assert!(!one.contains("1 projects"), "{one}");

        let two = vec![rows[0], rows[0]];
        let many = format_project_table(&two, 2, Some("1.2 GB"), 100, false).join("\n");
        assert!(many.contains("✓ 2 projects"), "{many}");
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

    /// The color gate is per-stream: a non-TTY stream never emits color.
    #[test]
    fn color_gate_is_off_for_non_tty_streams() {
        assert!(!color_enabled_for(false));
    }

    /// A direct terminal — a real pty, `stream_is_tty = true`, the case a test
    /// against piped stdout can never reach — emits color only when the
    /// terminal can render it. `TERM=dumb` is the canonical "cannot render
    /// escapes" signal (Emacs `M-x shell` is exactly this: a pty with
    /// `TERM=dumb`), so it degrades to plain text rather than printing SGR
    /// sequences the terminal shows literally.
    #[test]
    fn color_gate_is_off_on_a_dumb_terminal() {
        let dumb = std::ffi::OsString::from("dumb");
        let capable = std::ffi::OsString::from("xterm-256color");
        assert!(!color_enabled_with(true, Some(&dumb), false));
        assert!(color_enabled_with(true, Some(&capable), false));
        assert!(color_enabled_with(true, None, false));
        assert!(!color_enabled_with(false, Some(&capable), false));
        // The explicit conventions still win over a capable terminal.
        assert!(!color_enabled_with(true, Some(&capable), true));
    }

    /// The color gate and the rich-UI gate agree about which terminals can
    /// render escapes at all: whenever the rich presentation steps aside for an
    /// incapable terminal, the plain fallback it hands over to is plain in
    /// color too. Any divergence is escape-code garbage on screen.
    #[test]
    fn color_gate_never_outlives_the_rich_ui_gate() {
        let terms = [
            None,
            Some(std::ffi::OsString::from("dumb")),
            Some(std::ffi::OsString::from("xterm-256color")),
        ];
        for term in &terms {
            for stream_is_tty in [false, true] {
                for plain in [false, true] {
                    let term = term.as_deref();
                    assert!(
                        !color_enabled_with(stream_is_tty, term, plain)
                            || terminal_ui_enabled_with(stream_is_tty, term),
                        "tty {stream_is_tty}, TERM {term:?}: colored a terminal the rich UI skipped"
                    );
                }
            }
        }
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
            format_clean_review(Path::new("/w/dash"), "main", &review_rows, None, 40, false),
            format_blocked_project(
                Path::new("/w/dash"),
                Status::Wip,
                true,
                "main · uncommitted changes",
                &[" M src/a.ts".to_string()],
                Some("540 MB"),
                90,
                false,
            ),
            format_nothing_to_reclaim(Path::new("/w/dash"), "main · clean tree", 90, false),
            format_clean_success(Path::new("/w/dash"), 1, Some("1.2 GB"), 90, false),
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
