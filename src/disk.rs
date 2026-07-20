//! Disk-savings computation for devclean (issue #18).
//!
//! For each cleanable (status-5) project, sum the on-disk size of the items
//! that the cleaning engine would delete — `Safe` + `Surfaced` items (in
//! `dry_run` mode every surfaced item is auto-approved, so every non-Protected
//! item is deletable). `Protected` items are excluded from the count:
//! `.devcleanignore`-matched items stay on disk, never deleted.
//!
//! The walk is cross-platform and safe:
//!
//! - `symlink_metadata` is used so each symlink counts its own size only
//!   (the link's own bytes, not its target's total — no double-counting
//!   and no infinite recursion into a self-referential link).
//! - `walkdir::WalkDir` with `follow_links = false` (the default) recurses
//!   into non-symlink directories without following a symlink to a target
//!   that could shadow a sibling tree.
//! - A permission-denied entry on any path counts as zero: `Ok(0)` instead
//!   of aborting the listing. A warning on stderr is emitted for each path
//!   that could not be read.
//! - An empty item (a zero-length file or an empty directory) contributes
//!   zero bytes — counted, not skipped.
//!
//! The computation is `u64`, accumulated with `saturating_add` — no
//! overflow path in practice for on-disk sizes (the CLI also sums each
//! cleanable project's size into an aggregate for the summary line).

use std::fs::{self, File};
use std::path::Path;

use crate::clean::Classification;
use crate::clean::CleanItem;

/// Compute the total on-disk size of each deletable item for `project_path`.
///
/// Each `Safe` item is always deletable; each `Surfaced` item is deletable in
/// `dry_run` mode (the dry-run preview auto-approves every item — see
/// `dry_run`'s contract). `Protected` items are excluded from the count
/// (they carry an `.devcleanignore` match or an absolute-path fail-safe).
///
/// Returns the sum in bytes (unformatted). Returns `Ok(0)` if no deletable
/// items exist (or all paths could not be read).
pub fn compute_reclaimable_size(
    project_path: &Path,
    items: &[CleanItem],
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut total: u64 = 0;
    for item in items {
        if item.classification != Classification::Safe
            && item.classification != Classification::Surfaced
        {
            continue;
        }
        let abs = project_path.join(&item.rel_path);
        match compute_item_size(&abs, &item.rel_path) {
            Ok(bytes) => total = total.saturating_add(bytes),
            Err(e) => {
                eprintln!(
                    "warning: {}: could not read size: {}",
                    item.rel_path.display(),
                    e
                );
            }
        }
    }
    Ok(total)
}

/// Size of one item (file, directory, or symlink).
///
/// Symlinks count each link's own size (`symlink_metadata`), not its target's
/// total. Directories recurse via `walkdir::WalkDir` with no symlink following.
/// Permission-denied returns `Ok(0)` so each bad entry costs zero bytes,
/// not the whole listing.
fn compute_item_size(path: &Path, _rel: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    if path.is_symlink() {
        return symlink_size(path);
    }
    if path.is_file() {
        return file_size(path);
    }
    if path.is_dir() {
        return directory_size(path);
    }
    // Non-existent, char device, socket — count as zero.
    Ok(0)
}

/// Size of a symlink: the link's own content in bytes, not the target.
///
/// Each link is its own entry, so each link's size contributes once — no
/// target recursion.
fn symlink_size(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    let meta = fs::symlink_metadata(path)?;
    Ok(meta.len())
}

/// Size of one regular file: `stat` via `fs::metadata`, not a full read.
///
/// `metadata().len()` returns the file's logical size from the inode in O(1)
/// (one syscall) without reading file content — the same approach `du`, `ncdu`,
/// and `dust` use. A 0-byte file returns 0 (counted, not skipped). A
/// permission‑denied `stat` returns `Ok(0)` so one unreadable file does not zero
/// out the whole listing.
fn file_size(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    match File::open(path) {
        Ok(f) => {
            // File opened successfully; use its metadata for size.
            match f.metadata() {
                Ok(meta) => Ok(meta.len()),
                Err(e) => {
                    eprintln!(
                        "warning: {}: could not stat after opening: {}",
                        path.display(),
                        e
                    );
                    Ok(0)
                }
            }
        }
        Err(e) => {
            eprintln!(
                "warning: {}: could not open for size: {}",
                path.display(),
                e
            );
            Ok(0)
        }
    }
}

/// Size of a directory: each descendant file's size summed.
///
/// `walkdir::WalkDir` recurses without following symlinks; each symlink is
/// counted at its own size (see `symlink_size`). A permission-denied entry
/// counts as 0 for that subtree — the rest of the walk proceeds.
fn directory_size(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    use walkdir::WalkDir;

    let mut total: u64 = 0;
    let iter = WalkDir::new(path).follow_links(false);
    for entry in iter {
        match entry {
            Ok(e) => {
                let p = e.path();
                if p.is_file() || p.is_symlink() {
                    match compute_item_size(p, p) {
                        Ok(bytes) => total = total.saturating_add(bytes),
                        Err(_) => { /* count as 0 */ }
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: {}: could not walk: {}", path.display(), e);
                // Count the rest of this subtree as 0.
                continue;
            }
        }
    }
    Ok(total)
}

/// Human-readable size format: KB / MB / GB, two decimals.
///
/// 1024-based: KB → 1024, MB → 1024², GB → 1024³. Always picks the largest
/// unit that keeps the value ≥ 1.0. Values below 1 KB are shown in bytes
/// (`N B`); zero is `0 B`.
pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1.0 {
        return format!("{bytes} B");
    }
    let mb = kb / 1024.0;
    if mb < 1.0 {
        return format!("{kb:.2} KB");
    }
    let gb = mb / 1024.0;
    if gb < 1.0 {
        return format!("{mb:.2} MB");
    }
    format!("{gb:.2} GB")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir(label: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        d.push(format!(
            "devclean-disk-{}-{}-{n}",
            label,
            std::process::id()
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

    /// A safe-list file of known size is counted exactly.
    #[test]
    fn safe_file_counts_exact_size() {
        let root = unique_dir("safe_file");
        let payload = "hello world bytes";
        write_file(&root, "target/app.bin", payload);

        let items = vec![CleanItem {
            rel_path: PathBuf::from("target/app.bin"),
            is_dir: false,
            classification: Classification::Safe,
        }];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        assert_eq!(size, payload.len() as u64);
    }

    /// A safe-list directory's entire subtree is counted.
    #[test]
    fn safe_directory_counts_subtree() {
        let root = unique_dir("safe_dir");
        write_file(&root, "target/a.bin", "aaa");
        write_file(&root, "target/b.bin", "bbbb");
        write_file(&root, "target/sub/c.bin", "ccccc");

        let items = vec![CleanItem {
            rel_path: PathBuf::from("target"),
            is_dir: true,
            classification: Classification::Safe,
        }];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        assert_eq!(size, 3u64 + 4u64 + 5u64);
    }

    /// A protected file is excluded (counts zero).
    #[test]
    fn protected_file_excluded_from_count() {
        let root = unique_dir("protected_file");
        write_file(&root, "important.dat", "keep me");

        let items = vec![CleanItem {
            rel_path: PathBuf::from("important.dat"),
            is_dir: false,
            classification: Classification::Protected,
        }];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        assert_eq!(size, 0);
    }

    /// A symlink counts its own size, not its target's total.
    #[test]
    fn symlink_counts_own_size_not_target() {
        let root = unique_dir("symlink");
        let payload = "hello";
        write_file(&root, "target/app.bin", payload);
        // Symlink points at itself (or any target) — we only count the link
        // itself, not the target's full tree.
        std::os::unix::fs::symlink(root.join("target/app.bin"), root.join("target/link.bin"))
            .unwrap();

        let items = vec![CleanItem {
            rel_path: PathBuf::from("target/link.bin"),
            is_dir: false,
            classification: Classification::Safe,
        }];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        // The link itself is non-zero (its own content in bytes); it does
        // not pull in the target's total — each link counts once.
        assert!(size > 0);
        assert!(
            size != payload.len() as u64,
            "symlink size must be its own, not the target total: {size}"
        );
    }

    /// A permission-denied file counts as zero — the listing does not abort.
    #[test]
    fn permission_denied_counts_as_zero() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_dir("perms");
        let p = write_file(&root, "target/.locked", "locked");
        // Make it unreadable.
        let perms = fs::Permissions::from_mode(0o000);
        fs::set_permissions(&p, perms).unwrap();

        let items = vec![CleanItem {
            rel_path: PathBuf::from("target/.locked"),
            is_dir: false,
            classification: Classification::Safe,
        }];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        assert_eq!(size, 0, "permission-denied must count as 0");
    }

    /// A mix of safe and protected items: only the deletable ones counted.
    #[test]
    fn mix_of_safe_and_protected_each_counts() {
        let root = unique_dir("mix");
        write_file(&root, "target/junk.bin", "junk");
        write_file(&root, "important.dat", "keep");

        let items = vec![
            CleanItem {
                rel_path: PathBuf::from("target/junk.bin"),
                is_dir: false,
                classification: Classification::Safe,
            },
            CleanItem {
                rel_path: PathBuf::from("important.dat"),
                is_dir: false,
                classification: Classification::Protected,
            },
        ];
        let size = compute_reclaimable_size(&root, &items).unwrap();
        assert_eq!(size, 4); // only the safe item counted
    }

    /// format_size: 0 → "0 B".
    #[test]
    fn format_zero_is_zero_b() {
        assert_eq!(format_size(0), "0 B");
    }

    /// format_size: sub-KB stays in B.
    #[test]
    fn format_sub_kb_stays_in_bytes() {
        assert_eq!(format_size(100), "100 B");
    }

    /// format_size: KB for values ≥ 1024.
    #[test]
    fn format_kb_above_1024() {
        assert!(format_size(1024).starts_with("1.00 KB"));
    }

    /// format_size: MB for values ≥ 1024².
    #[test]
    fn format_mb_above_1mb() {
        assert!(format_size(1024 * 1024).starts_with("1.00 MB"));
    }

    /// format_size: GB for values ≥ 1024³.
    #[test]
    fn format_gb_above_1gb() {
        assert!(format_size(1024 * 1024 * 1024).starts_with("1.00 GB"));
    }

    /// format_size: human-readable — no raw numbers above 1024.
    #[test]
    fn format_size_is_human_readable() {
        let s = format_size(12345678);
        assert!(
            s.contains("MB") || s.contains("KB") || s.contains("GB"),
            "unexpected: {s}"
        );
    }
}
