//! Live progress indicator for discovery, classification, sizing, and cleaning
//! phases.
//!
//! Each time walk_root visits a directory, this module renders the live
//! discovery state: current path, activity bar, scanned directory count, found
//! project count, indexed bytes, and a bounded recent-project list. The panel
//! overwrites itself in place on a capable terminal, so the display does not
//! scroll while still showing the reference artifact's "discovered so far"
//! state. Extremely narrow terminals fall back to the old single `walking:`
//! line because the full panel cannot carry useful detail without wrapping.
//!
//! The same writer also emits counted single-line phase labels during
//! classification, sizing, reading, and cleaning: each project classified, each
//! cleanable project sized, or each cleanable project cleaned gets
//! `classifying N/M: <project>`, `sizing N/M: <project>`, or
//! `cleaning N/M: <project>` rendered in place, so the screen is never blank
//! during long git and filesystem work.
//!
//! ## Terminal UI gating
//!
//! Renders only when stdout passes output::stdout_terminal_ui_enabled(): a
//! capable terminal UI on stdout. When piped, redirected, or running under
//! TERM=dumb (even on a TTY), nothing is emitted -- a stream of CR-terminated
//! partial paths or ANSI cursor controls would be garbage in a pipe, log file,
//! or dumb terminal. Reuses the shared rich-output capability gate; no
//! duplicate detection.
//!
//! ## Truncation
//!
//! A path, fixed progress label, or panel row that wraps to a second line would
//! scroll and defeat the live-display purpose. The module picks terminal width
//! via terminal_size (lightweight, no ANSI escapes), falling back to the COLUMNS
//! env var, then a default of 80. Labels are clipped first; paths are appended
//! only when a label leaves budget, and truncated with a leading ellipsis
//! (U+2026) so the leaf (the dir currently being visited) stays visible on the
//! right. Fit is measured in display columns via unicode-width -- wide chars
//! (CJK, emoji) occupy two columns each -- never in bytes or chars, or wide
//! paths would wrap and scroll. Each single-line update pads with spaces and CR
//! so a shorter path fully overwrites a longer previous one; multi-line
//! discovery refreshes clear each old row before writing the next panel.
//!
//! ## Clear and finish
//!
//! Whenever other output (a summary line, a per-project block, a stderr
//! warning) must interleave with a live progress line, the line is first
//! cleared in place (CR + spaces to width + CR, no newline) via `clear` so
//! the following output overwrites it rather than wrapping after the padded
//! line. When a phase completes, `finish` clears the line and emits a
//! newline so the following summary line (e.g. listing: N projects) starts
//! on a fresh line. No partial line lingers above the summary.
//!
//! ## Testability
//!
//! ProgressWriter is generic over any Write implementor so unit tests can
//! inject a Vec<u8> buffer instead of a real terminal. No real TTYs are
//! spawned in tests; fake active writers cover the terminal-rendering seam.

use std::io::Write;
use std::path::Path;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::output;

/// Live progress writer for discovery, classification, and cleaning phases.
///
/// Generic over W: Write so tests can inject a Vec<u8> buffer instead of
/// a real terminal. On a TTY, each update writes CR-prefixed text with no
/// trailing newline; each finish clears the line and emits a newline.
pub struct ProgressWriter<W: Write> {
    writer: W,
    width: usize,
    /// Whether stdout supports the terminal UI. Gated on
    /// output::stdout_terminal_ui_enabled() at construction time -- the struct
    /// never renders when stdout is not a capable terminal, including TERM=dumb.
    active: bool,
    tick: usize,
    live_rows: usize,
}

pub const DISCOVERY_RECENT_LIMIT: usize = 6;
const DISCOVERY_PANEL_MIN_WIDTH: usize = 32;

/// One project row known during discovery. Discovery knows only the marker
/// match, not later git status or reclaimable size.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryProgressProject<'a> {
    pub path: &'a Path,
    pub marker: &'a str,
}

/// The live discovery state rendered while workspace roots are walked.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryProgress<'a> {
    pub current_path: &'a Path,
    pub dirs_scanned: usize,
    pub projects_found: usize,
    pub indexed_bytes: u64,
    pub recent_projects: &'a [DiscoveryProgressProject<'a>],
}

impl<W: Write> ProgressWriter<W> {
    /// Build a progress writer against writer, checking the terminal UI gate.
    ///
    /// If stdout supports output::stdout_terminal_ui_enabled(), active is true
    /// and each update renders; otherwise, including TERM=dumb on a TTY, each
    /// update is a no-op. Terminal width is resolved via terminal_size, then
    /// COLUMNS, then a default of 80.
    pub fn new(writer: W) -> Self {
        let active = output::stdout_terminal_ui_enabled();
        let width = terminal_width();
        Self {
            writer,
            width,
            active,
            tick: 0,
            live_rows: 0,
        }
    }

    /// Emit the current directory path on one line, overwriting the previous
    /// line in place. No trailing newline -- each call replaces the prior one.
    ///
    /// When not a TTY this is a no-op: the struct's active flag short-circuits
    /// the path formatting and the write.
    pub fn update(&mut self, path: &Path) {
        if !self.active {
            return;
        }
        let label = format!("{} walking: ", self.spinner());
        self.tick = self.tick.wrapping_add(1);
        let line = render_line(label, path, self.width);
        self.write_live_lines(&[line]);
    }

    /// Emit the richer discovery-scanning state: current path, activity bar,
    /// live counts, indexed bytes, and the bounded recent project list.
    ///
    /// This uses ANSI cursor-up/line-clear sequences only after the same
    /// capable-terminal gate as the rest of the rich terminal UI. Piped output,
    /// redirected output, and `TERM=dumb` remain no-ops.
    pub fn update_discovery(&mut self, state: DiscoveryProgress<'_>) {
        if !self.active {
            return;
        }
        if self.width < DISCOVERY_PANEL_MIN_WIDTH {
            self.update(state.current_path);
            return;
        }
        let spinner = self.spinner();
        self.tick = self.tick.wrapping_add(1);
        let lines = render_discovery_panel(state, spinner, self.width);
        self.write_live_lines(&lines);
    }

    /// Emit a counted phase label with a path, overwriting the previous line
    /// in place. No trailing newline -- each call replaces the prior one.
    ///
    /// Renders `<phase> <idx>/<total>: <path>` with the same CR-overwrite,
    /// truncation (leading ellipsis, leaf visible), TTY-gating, and padding
    /// as `update`. The label is caller-supplied so the same struct can show
    /// `classifying 3/47: <project>` or `cleaning 1/5: <project>` without
    /// duplicating the writer.
    ///
    /// `idx` is 1-based (first item is 1, not 0) so the reader sees natural
    /// enumeration. `total` is the total number of items in the phase.
    ///
    /// When not a TTY this is a no-op: the struct's active flag short-circuits
    /// the path formatting and the write.
    pub fn update_phase(&mut self, phase: &str, idx: usize, total: usize, path: &Path) {
        if !self.active {
            return;
        }
        let label = format!("{} {} {}/{}: ", self.spinner(), phase, idx, total);
        self.tick = self.tick.wrapping_add(1);
        let line = render_line(&label, path, self.width);
        self.write_live_lines(&[line]);
    }

    /// Clear the progress line in place: CR + spaces to width + CR, no
    /// newline. The cursor lands at column 0 of the erased line so the next
    /// write (a println, a stderr warning) overwrites it rather than
    /// wrapping after the padded progress line.
    ///
    /// When not a TTY this is a no-op.
    pub fn clear(&mut self) {
        if !self.active {
            return;
        }
        if self.live_rows <= 1 {
            // Clear: CR + spaces to width + CR. Portable for the single-line
            // progress phases.
            let _ = self.writer.write_all(b"\r");
            let spaces = " ".repeat(self.width);
            let _ = self.writer.write_all(spaces.as_bytes());
            let _ = self.writer.write_all(b"\r");
        } else {
            for i in 0..self.live_rows {
                let _ = self.writer.write_all(b"\r\x1b[2K");
                if i + 1 < self.live_rows {
                    let _ = self.writer.write_all(b"\x1b[1A");
                }
            }
        }
        self.live_rows = 0;
        let _ = self.writer.flush();
    }

    /// Clear the progress line and emit a newline so the following summary
    /// line starts on a fresh line.
    ///
    /// When not a TTY this is a no-op.
    pub fn finish(&mut self) {
        if !self.active {
            return;
        }
        self.clear();
        let _ = self.writer.write_all(b"\n");
        let _ = self.writer.flush();
    }

    fn spinner(&self) -> &'static str {
        const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[self.tick % FRAMES.len()]
    }

    fn write_live_lines(&mut self, lines: &[String]) {
        if self.live_rows > 0 {
            self.clear();
        } else {
            let _ = self.writer.write_all(b"\r");
        }
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                let _ = self.writer.write_all(b"\n");
            }
            let _ = self.writer.write_all(line.as_bytes());
        }
        self.live_rows = lines.len();
        let _ = self.writer.flush();
    }
}

/// Resolve terminal width: terminal_size first, then COLUMNS env var,
/// then a default of 80.
///
/// This module owns the fallback rule for the whole crate — `output` renders
/// its panels and tables against this same function rather than repeating the
/// chain.
pub fn terminal_width() -> usize {
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        return w as usize;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(80)
}

/// Render a single-line progress string: label + truncated path, padded to
/// terminal width in display columns. Shared by `update` and `update_phase`
/// so both callers reuse the same truncation and padding logic.
///
/// The path is right-aligned (leaf visible on the right) with a leading
/// ellipsis (U+2026) when it exceeds the remaining columns after the label.
/// Wide chars (CJK, emoji) are counted in display columns, never in bytes.
/// The returned string is padded with trailing spaces to `width` so each
/// update fully overwrites a longer previous one.
fn render_line(label: impl AsRef<str>, path: &Path, width: usize) -> String {
    let label = clip_left(label.as_ref(), width);
    let display = path.display().to_string();
    let max_cols = width.saturating_sub(label.width());
    let truncated = if max_cols == 0 {
        String::new()
    } else if display.width() > max_cols {
        let ellipsis = "\u{2026}";
        // Right-align: the leaf (current dir) stays visible. We keep chars
        // from the right whose total display width fits, accounting for
        // the ellipsis at the left.
        let keep = max_cols.saturating_sub(ellipsis.width());
        let mut cols = 0;
        let mut start = display.len();
        for (idx, ch) in display.char_indices().rev() {
            let ch_cols = ch.width().unwrap_or(0);
            if cols + ch_cols > keep {
                break;
            }
            cols += ch_cols;
            start = idx;
        }
        format!("{}{}", ellipsis, &display[start..])
    } else {
        display
    };
    let line = format!("{}{}", label, truncated);
    // Pad with spaces so a shorter path fully overwrites a longer previous
    // one (no leftover trailing characters from the prior line). Measured
    // in display columns: wide chars (CJK, emoji) occupy two columns each.
    let pad = width.saturating_sub(line.width());
    format!("{}{}", line, " ".repeat(pad))
}

fn render_discovery_panel(
    state: DiscoveryProgress<'_>,
    spinner: &str,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(fit_line(
        &format!("{spinner} scanning {}", activity_bar(width, spinner)),
        width,
    ));
    lines.push(render_line("walking ", state.current_path, width));
    lines.push(fit_line(
        &format!(
            "{} dirs scanned · {} projects found · {} indexed",
            format_count(state.dirs_scanned),
            format_count(state.projects_found),
            format_indexed_size(state.indexed_bytes)
        ),
        width,
    ));
    lines.push(fit_line("Discovered so far", width));

    let start = state
        .recent_projects
        .len()
        .saturating_sub(DISCOVERY_RECENT_LIMIT);
    for project in &state.recent_projects[start..] {
        lines.push(fit_line(
            &format!(
                "✓ {} ({})",
                discovery_project_label(project.path),
                project.marker
            ),
            width,
        ));
    }

    lines.push(fit_line(
        "press ctrl-c to stop · results appear as folders finish indexing",
        width,
    ));
    lines
}

fn activity_bar(width: usize, spinner: &str) -> String {
    let bar_width = width.saturating_sub(spinner.width() + " scanning ".width());
    if bar_width < 3 {
        return String::new();
    }
    let inner = bar_width.saturating_sub(2).min(24);
    let frames = ["=>", "==>", "===>", "====>", "=====>", "======>"];
    let idx = spinner.chars().next().map(|c| c as usize).unwrap_or(0) % frames.len();
    let fill = frames[idx];
    let body = if fill.width() >= inner {
        truncate_cols(fill, inner)
    } else {
        format!("{fill}{}", " ".repeat(inner - fill.width()))
    };
    format!("[{body}]")
}

fn discovery_project_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn format_count(count: usize) -> String {
    let raw = count.to_string();
    let mut out = String::new();
    for (i, ch) in raw.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_indexed_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn fit_line(s: &str, width: usize) -> String {
    truncate_cols(s, width)
}

fn truncate_cols(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut cols = 0;
    for ch in s.chars() {
        let ch_cols = ch.width().unwrap_or(0);
        if cols + ch_cols > width {
            break;
        }
        out.push(ch);
        cols += ch_cols;
    }
    out
}

fn clip_left(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut cols = 0;
    for ch in s.chars() {
        let ch_cols = ch.width().unwrap_or(0);
        if cols + ch_cols > width {
            break;
        }
        out.push(ch);
        cols += ch_cols;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The progress writer is a no-op when not a TTY: update writes nothing,
    /// finish writes nothing. Inject a non-TTY writer (a Vec<u8> buffer).
    #[test]
    fn no_op_when_not_tty() {
        let buf: Vec<u8> = Vec::new();
        // Construct a ProgressWriter with active = false (simulating non-TTY).
        let mut pw = ProgressWriter {
            writer: buf,
            width: 80,
            active: false,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/some/deep/path"));
        pw.finish();
        assert!(pw.writer.is_empty(), "non-TTY writer must emit nothing");
    }

    /// On a fake TTY writer (buffer with active = true), update emits
    /// CR-prefixed single-line updates with no trailing newline, and
    /// truncates long paths with a leading ellipsis.
    #[test]
    fn updates_on_fake_tty() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 40,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/some/deep/path"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        // Starts with CR, no trailing newline.
        assert!(
            bytes.starts_with("\r"),
            "update must start with CR: {:?}",
            bytes
        );
        assert!(
            !bytes.ends_with('\n'),
            "update must not end with newline: {:?}",
            bytes
        );
        // Contains "walking: " label.
        assert!(
            bytes.contains("walking: "),
            "update must contain label: {:?}",
            bytes
        );
        // Short path fits; no ellipsis.
        assert!(
            !bytes.contains('\u{2026}'),
            "short path must not be ellipsized: {:?}",
            bytes
        );
    }

    /// Long paths are truncated with a leading ellipsis so they never wrap.
    #[test]
    fn truncates_long_paths_with_leading_ellipsis() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        // Path longer than width minus label.
        pw.update(Path::new("/very/deep/nested/project/structure/here"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        // Must contain the ellipsis.
        assert!(
            bytes.contains('\u{2026}'),
            "long path must be ellipsized: {:?}",
            bytes
        );
        // The leaf (last segment) stays visible on the right.
        assert!(
            bytes.contains("here"),
            "leaf must stay visible: {:?}",
            bytes
        );
        // No trailing newline.
        assert!(
            !bytes.ends_with('\n'),
            "update must not end with newline: {:?}",
            bytes
        );
    }

    /// Truncating a path with multibyte UTF-8 segments must not panic: the
    /// slice start is snapped forward to the next char boundary.
    #[test]
    fn truncates_multibyte_paths_without_panicking() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        // With width 20 the slice start lands mid-character in the CJK leaf,
        // so this panics unless the start is snapped to a char boundary.
        pw.update(Path::new("/Users/séb/工程/项目文件夹"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            bytes.contains('\u{2026}'),
            "long multibyte path must be ellipsized: {:?}",
            bytes
        );
        assert!(
            bytes.contains("件夹"),
            "leaf tail must stay visible: {:?}",
            bytes
        );
    }

    /// Wide chars (CJK, emoji) occupy two display columns each; the rendered
    /// line must be truncated and padded in columns, never exceeding the
    /// terminal width, or it would wrap and scroll on every update.
    #[test]
    fn wide_char_line_fits_terminal_width_in_columns() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 30,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/Users/dev/工程目录/项目文件夹的名称很长"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        let segment = bytes.rsplit('\r').next().unwrap();
        assert_eq!(
            segment.width(),
            30,
            "rendered line must be padded to exactly the terminal width in \
             display columns: {:?}",
            segment
        );
    }

    /// A terminal narrower than the label plus the ellipsis must not underflow
    /// or panic; the fixed label itself is clipped to the available columns.
    #[test]
    fn tiny_width_does_not_panic() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 10,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/some/deep/path"));
        pw.finish();
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            bytes.contains("walking:"),
            "width 10 can still carry the walking label: {:?}",
            bytes
        );
    }

    /// Even when COLUMNS is smaller than the spinner/label itself, the live
    /// progress line must stay on one row. Paths and ellipses are appended
    /// only after the clipped label leaves budget.
    #[test]
    fn progress_lines_fit_tiny_widths() {
        for width in 0..=12 {
            for line in [
                render_line("⠋ walking: ", Path::new("/some/deep/project"), width),
                render_line("⠙ sizing 1/1: ", Path::new("/some/deep/project"), width),
            ] {
                assert!(
                    line.width() <= width,
                    "width {width}: rendered {} columns: {line:?}",
                    line.width()
                );
                if width < "⠋ walking: ".width() {
                    assert!(
                        !line.contains('/'),
                        "path must not be appended until the label leaves budget: {line:?}"
                    );
                }
            }
        }
    }

    /// clear erases the line in place without emitting a newline, so the
    /// next write starts at column 0 of the erased line.
    #[test]
    fn clear_erases_line_without_newline() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/some/path"));
        pw.clear();
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            !bytes.contains('\n'),
            "clear must not emit a newline: {:?}",
            bytes
        );
        assert!(
            bytes.ends_with('\r'),
            "clear must leave the cursor at column 0: {:?}",
            bytes
        );
        let last = bytes.split('\r').rev().nth(1).unwrap_or("");
        assert!(
            last.trim().is_empty(),
            "cleared segment must be spaces only: {:?}",
            last
        );
    }

    /// clear is a no-op when not a TTY.
    #[test]
    fn clear_no_op_when_not_tty() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: false,
            live_rows: 0,
            tick: 0,
        };
        pw.clear();
        assert!(pw.writer.is_empty(), "non-TTY clear must emit nothing");
    }

    /// finish clears the line and emits a newline.
    #[test]
    fn finish_clears_line_and_emits_newline() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update(Path::new("/some/path"));
        pw.finish();
        let bytes = String::from_utf8_lossy(&pw.writer);
        // Must end with a newline.
        assert!(
            bytes.ends_with('\n'),
            "finish must emit a newline: {:?}",
            bytes
        );
        // The cleared line contains CRs and spaces before the final newline.
        assert!(bytes.contains("\r"), "finish must contain CR: {:?}", bytes);
    }

    /// Each update overwrites the previous line: a shorter path after a longer
    /// one produces a buffer where each CR-delimited segment contains only
    /// its own content -- no leftover trailing characters from the prior line
    /// appear within a segment.
    #[test]
    fn shorter_path_overwrites_longer_previous() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 40,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        // First update: long path (within width).
        pw.update(Path::new("/a/very/deep/nested/project/structure"));
        // Second update: shorter path.
        pw.update(Path::new("/short"));
        let second = String::from_utf8_lossy(&pw.writer);
        // Each segment between CR characters contains only its own content.
        // The second segment does not carry trailing chars from the first.
        assert!(
            second.contains("walking: /short"),
            "second update must show the new path: {:?}",
            second
        );
        // Split on CR and check each segment.
        let segments: Vec<&str> = second.split('\r').collect();
        // Last segment is the final newline (empty or just newline).
        let last = segments.last().copied().unwrap_or("");
        assert!(
            !last.contains("structure"),
            "final segment must not carry trailing chars: {:?}",
            last
        );
    }

    /// update_phase emits <phase> <idx>/<total>: <path> with CR prefix, no
    /// trailing newline, and the same truncation/padding as update.
    #[test]
    fn update_phase_emits_phase_label_and_counter() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 40,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update_phase("classifying", 3, 47, Path::new("/my/project"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        // Starts with CR, no trailing newline.
        assert!(bytes.starts_with("\r"), "update_phase must start with CR");
        assert!(
            !bytes.ends_with('\n'),
            "update_phase must not end with newline"
        );
        // Contains the phase label, counter, and path.
        assert!(
            bytes.contains("classifying 3/47: /my/project"),
            "update_phase must contain phase/counter/path: {:?}",
            bytes
        );
    }

    /// update_phase is a no-op when not a TTY.
    #[test]
    fn update_phase_no_op_when_not_tty() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 80,
            active: false,
            live_rows: 0,
            tick: 0,
        };
        pw.update_phase("cleaning", 1, 5, Path::new("/a/b/c"));
        pw.finish();
        assert!(
            pw.writer.is_empty(),
            "non-TTY update_phase must emit nothing: {:?}",
            pw.writer
        );
    }

    /// update_phase truncates long paths with a leading ellipsis, just like
    /// update. The leaf (last segment) stays visible on the right.
    #[test]
    fn update_phase_truncates_long_paths() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 35,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        // Path longer than width minus the phase/counter label.
        pw.update_phase(
            "classifying",
            1,
            100,
            Path::new("/very/deep/nested/project/structure/here"),
        );
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(bytes.contains('\u{2026}'), "long path must be ellipsized");
        assert!(bytes.contains("here"), "leaf must stay visible");
        assert!(
            !bytes.ends_with('\n'),
            "update_phase must not end with newline"
        );
    }

    /// update_phase with multibyte UTF-8 path must not panic: the slice start
    /// is snapped forward to the next char boundary.
    #[test]
    fn update_phase_truncates_multibyte_paths() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 25,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update_phase("cleaning", 1, 3, Path::new("/Users/séb/工程/项目文件夹"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            bytes.contains('\u{2026}'),
            "long multibyte path must be ellipsized"
        );
        assert!(bytes.contains("件夹"), "leaf tail must stay visible");
    }

    /// Each update_phase call overwrites the previous one: a shorter path
    /// after a longer one produces a buffer where each CR-delimited segment
    /// contains only its own content.
    #[test]
    fn update_phase_overwrites_previous() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 40,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update_phase(
            "classifying",
            1,
            5,
            Path::new("/a/very/deep/nested/project/structure"),
        );
        pw.update_phase("classifying", 2, 5, Path::new("/short"));
        let second = String::from_utf8_lossy(&pw.writer);
        assert!(
            second.contains("classifying 2/5: /short"),
            "second update_phase must show the new path: {:?}",
            second
        );
        let segments: Vec<&str> = second.split('\r').collect();
        let last = segments.last().copied().unwrap_or("");
        assert!(
            !last.contains("structure"),
            "final segment must not carry trailing chars: {:?}",
            last
        );
    }

    /// update_phase renders the phase label exactly as supplied -- callers
    /// can use "classifying", "cleaning", or any other label.
    #[test]
    fn update_phase_renders_supplied_label() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 40,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        pw.update_phase("cleaning", 1, 3, Path::new("/project"));
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            bytes.contains("cleaning 1/3: /project"),
            "must render the supplied label: {:?}",
            bytes
        );
    }

    #[test]
    fn discovery_panel_shows_scanning_counts_indexed_bytes_and_recent_projects() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 96,
            active: true,
            live_rows: 0,
            tick: 0,
        };
        let recent = [
            DiscoveryProgressProject {
                path: Path::new("/workspace/dashboard"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/api-gateway"),
                marker: "go.mod",
            },
        ];

        pw.update_discovery(DiscoveryProgress {
            current_path: Path::new("/workspace/api-gateway/dist"),
            dirs_scanned: 1284,
            projects_found: 7,
            indexed_bytes: 2_900_000_000,
            recent_projects: &recent,
        });

        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(bytes.starts_with('\r'), "panel starts live: {bytes:?}");
        assert!(!bytes.ends_with('\n'), "panel stays live: {bytes:?}");
        assert!(bytes.contains("scanning"), "{bytes}");
        assert!(bytes.contains("walking"), "{bytes}");
        assert!(bytes.contains("/workspace/api-gateway/dist"), "{bytes}");
        assert!(bytes.contains("1,284 dirs scanned"), "{bytes}");
        assert!(bytes.contains("7 projects found"), "{bytes}");
        assert!(bytes.contains("2.7 GB indexed"), "{bytes}");
        assert!(bytes.contains("Discovered so far"), "{bytes}");
        assert!(bytes.contains("dashboard"), "{bytes}");
        assert!(bytes.contains(".git"), "{bytes}");
        assert!(bytes.contains("api-gateway"), "{bytes}");
        assert!(bytes.contains("go.mod"), "{bytes}");
        assert!(bytes.contains("ctrl-c"), "{bytes}");
    }

    #[test]
    fn discovery_panel_lines_fit_every_terminal_width() {
        let recent = [
            DiscoveryProgressProject {
                path: Path::new("/workspace/really/deep/dashboard"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/really/deep/api-gateway"),
                marker: "package.json",
            },
        ];

        for width in 0..=96 {
            for line in render_discovery_panel(
                DiscoveryProgress {
                    current_path: Path::new(
                        "/workspace/really/deep/api-gateway/node_modules/cache",
                    ),
                    dirs_scanned: 1284,
                    projects_found: 7,
                    indexed_bytes: 2_900_000_000,
                    recent_projects: &recent,
                },
                "⠋",
                width,
            ) {
                assert!(
                    line.width() <= width,
                    "width {width}: rendered {} columns: {line:?}",
                    line.width()
                );
            }
        }
    }

    #[test]
    fn discovery_panel_bounds_recent_projects() {
        let recent = [
            DiscoveryProgressProject {
                path: Path::new("/workspace/p1"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p2"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p3"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p4"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p5"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p6"),
                marker: ".git",
            },
            DiscoveryProgressProject {
                path: Path::new("/workspace/p7"),
                marker: ".git",
            },
        ];

        let rendered = render_discovery_panel(
            DiscoveryProgress {
                current_path: Path::new("/workspace/p7"),
                dirs_scanned: 70,
                projects_found: 7,
                indexed_bytes: 700,
                recent_projects: &recent,
            },
            "⠋",
            80,
        )
        .join("\n");

        assert!(!rendered.contains("p1"), "{rendered}");
        for project in ["p2", "p3", "p4", "p5", "p6", "p7"] {
            assert!(rendered.contains(project), "{rendered}");
        }
    }
}
