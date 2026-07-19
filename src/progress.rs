//! Live single-line progress indicator for the discovery walk.
//!
//! Each time walk_root visits a directory, this module renders the current
//! path on one line that overwrites itself in place via a carriage return
//! (CR), so the display never scrolls -- a TUI-style spinner that tells the
//! user devclean is working on a large workspace rather than appearing hung.
//!
//! ## TTY gating
//!
//! Renders only when stdout is a TTY (output::is_tty()). When piped or
//! redirected, nothing is emitted -- a stream of CR-terminated partial paths
//! would be garbage in a pipe or log file. Reuses the existing output::is_tty
//! gate; no duplicate detection.
//!
//! ## Truncation
//!
//! A path that wraps to a second line would scroll and defeat the single-line
//! purpose. The module picks terminal width via terminal_size (lightweight,
//! no ANSI escapes), falling back to the COLUMNS env var, then a default of
//! 80. Paths are truncated to fit with a leading ellipsis (U+2026) so the
//! leaf (the dir currently being visited) stays visible on the right. Fit is
//! measured in display columns via unicode-width -- wide chars (CJK, emoji)
//! occupy two columns each -- never in bytes or chars, or wide paths would
//! wrap and scroll. Each update pads with spaces and CR so a shorter path
//! fully overwrites a longer previous one (no leftover trailing characters).
//!
//! ## Finish
//!
//! When the walk completes, the progress line is cleared (CR + spaces to
//! width + CR) and a newline is emitted so the following summary line
//! (e.g. listing: N projects) starts on a fresh line. No partial line
//! lingers above the summary.
//!
//! ## Testability
//!
//! ProgressWriter is generic over any Write implementor so unit tests can
//! inject a Vec<u8> buffer instead of a real terminal. No real TTYs are
//! spawned in tests.

use std::io::Write;
use std::path::Path;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::output;

/// Live single-line progress writer for the discovery walk.
///
/// Generic over W: Write so tests can inject a Vec<u8> buffer instead of
/// a real terminal. On a TTY, each update writes CR-prefixed text with no
/// trailing newline; each finish clears the line and emits a newline.
pub struct ProgressWriter<W: Write> {
    writer: W,
    width: usize,
    /// Whether the writer is a TTY. Gated on output::is_tty() at construction
    /// time -- the struct never renders when not a TTY.
    active: bool,
}

impl<W: Write> ProgressWriter<W> {
    /// Build a progress writer against writer, checking the TTY gate.
    ///
    /// If stdout is a TTY, active is true and each update renders;
    /// otherwise each update is a no-op. Terminal width is resolved via
    /// terminal_size, then COLUMNS, then a default of 80.
    pub fn new(writer: W) -> Self {
        let active = output::is_tty();
        let width = terminal_width();
        Self {
            writer,
            width,
            active,
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
        let label = "walking: ";
        let display = path.display().to_string();
        let max_cols = self.width.saturating_sub(label.width());
        let truncated = if display.width() > max_cols {
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
        let pad = self.width.saturating_sub(line.width());
        let padded = format!("{}{}", line, " ".repeat(pad));
        // CR-prefixed, no trailing newline, flushed immediately.
        let _ = self.writer.write_all(b"\r");
        let _ = self.writer.write_all(padded.as_bytes());
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
        // Clear: CR + spaces to width + CR. Portable -- no ANSI escapes
        // (the crate avoids them outside owo-colors).
        let _ = self.writer.write_all(b"\r");
        let spaces = " ".repeat(self.width);
        let _ = self.writer.write_all(spaces.as_bytes());
        let _ = self.writer.write_all(b"\r");
        let _ = self.writer.write_all(b"\n");
        let _ = self.writer.flush();
    }
}

/// Resolve terminal width: terminal_size first, then COLUMNS env var,
/// then a default of 80.
fn terminal_width() -> usize {
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        return w as usize;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(80)
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
    /// or panic; the update degrades to the ellipsis alone.
    #[test]
    fn tiny_width_does_not_panic() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 10,
            active: true,
        };
        pw.update(Path::new("/some/deep/path"));
        pw.finish();
        let bytes = String::from_utf8_lossy(&pw.writer);
        assert!(
            bytes.contains("walking: "),
            "label still emitted: {:?}",
            bytes
        );
    }

    /// finish clears the line and emits a newline.
    #[test]
    fn finish_clears_line_and_emits_newline() {
        let buf: Vec<u8> = Vec::new();
        let mut pw = ProgressWriter {
            writer: buf,
            width: 20,
            active: true,
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
}
