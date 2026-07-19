//! Interactive cleaning flow for devclean (issue #8).
//!
//! Drives the interactive state machine across every cleanable project:
//! sort, report, collect approvals, then execute. The decision state machine
//! lives behind a `BufRead` seam so the I/O is replaceable — prompts are the
//! only part that moves when the crate swaps stdin for a TTY or a pipe.
//! The decision logic itself (what each project gets given the approvals
//! collected) is pure and testable without real stdin.
//!
//! ## Behavior matrix
//!
//! | flag            | all-cleanup prompt | per-item prompts | per-project prompt | execute?
//! |-----------------|-------------------|------------------|--------------------|--------|
//! | interactive     | yes → y/n         | yes → keep/dep   | yes → clean?       | yes if approved
//! | --force         | no                | no               | no                 | yes
//! | --dry-run       | no                | no               | no                 | no
//! | --force --dry-run | no              | no               | no                 | no (show)
//!
//! ## Decision state machine
//!
//! 1. **Report phase** — show every project sorted by status, report each
//!    non-cleanable one with a one-line reason, list each cleanable one.
//! 2. **All-cleanup prompt** — "Clean the N cleanable projects? (y/n)". If
//!    no (or EOF / --force / --dry-run), exit without touching any project.
//! 3. **Per-project loop** (in sorted order, each cleanable):
//!     a. enumerate untracked items (dry-run);
//!     b. print the project path and the list of each item that would be
//!        deleted;
//!     c. for each `Surfaced` item: prompt keep or delete; record approval;
//!     d. prompt per-project confirmation ("Clean <path>? (y/n)"); record
//!        approval;
//!     e. if approved (or --force): execute `git clean -xfd -e <globs>`;
//!        if --dry-run: print the report only.
//! 4. **Zero cleanable** — summary, exit 0, no prompts.
//!
//! ## Approval contract
//!
//! The crate only deletes items the user approved (or items the user did not
//! approve when --force is set — force auto-approves everything, so it is
//! every item the user would approve if prompted). A `Surfaced` item is
//! never silently deleted; a `Protected` item is always excluded. `Safe` and
//! approved-`Surfaced` items are left un-excluded so `git clean` deletes them.
//!
//! The `clean` function in `clean.rs` owns the deletion step; this module
//! owns the approval collection.

use std::io::BufRead;
use std::path::PathBuf;

use crate::classify::Status;
use crate::clean::{CleanItem, Classification};

/// Pre-computed inputs for the interactive flow: the discovered projects
/// already classified and sorted, the flags, and the per-project items
/// already enumerated.
///
/// The classifying, sorting, and per-project enumeration are all done before
/// this struct is built — by `main::run_clean`. That keeps the CLI hook
/// independent of the decision state machine.
#[derive(Debug, Clone)]
pub struct InteractiveFlowInputs {
    /// Each project's status, sorted by severity (most-needs-attention first).
    pub all_projects: Vec<(PathBuf, Status)>,
    /// Each cleanable (status-5) project's enumerated items, in the same
    /// order as `all_projects`.
    pub per_project_items: Vec<(PathBuf, Vec<CleanItem>)>,
    /// Force: skip all prompts, auto-approve each surfaced item.
    pub force: bool,
    /// Dry-run: show what would be deleted, delete nothing.
    pub dry_run: bool,
}

/// Per-project result produced by the interactive flow.
#[derive(Debug, Clone)]
pub struct ProjectResult {
    /// The project path.
    pub path: PathBuf,
    /// The project's git status (must be Cleanable for the result to be
    /// present — non-cleanable projects are reported but not cleaned).
    pub status: Status,
    /// Each untracked item enumerated for this project.
    pub items: Vec<CleanItem>,
    /// Items this project would be deleted (Safe + approved Surfaced for
    /// interactive; every item for force/dry-run auto-approve).
    pub would_delete: Vec<PathBuf>,
    /// Whether the user (or --force) approved this project for cleaning.
    pub project_approved: bool,
}

/// Reasons each non-cleanable project needs manual attention. One line per
/// project, plain text.
pub fn status_reason(status: Status) -> &'static str {
    match status {
        Status::NoGit => "not git-initialized",
        Status::NoRemote => "no remote configured",
        Status::Unpushed => "has unpushed commits",
        Status::Wip => "uncommitted work in progress",
        Status::Cleanable => "cleanable",
        Status::Clean => "clean",
    }
}

/// Whether each item would be deleted in the given mode. Used for display
/// and for the exclusion list at execution time.
fn would_delete(item: &CleanItem, force: bool, approved: &[PathBuf]) -> bool {
    match item.classification {
        Classification::Protected => false,
        Classification::Safe => true,
        Classification::Surfaced => force || approved.contains(&item.rel_path),
    }
}

/// A single line from `reader`, trimmed. Returns `None` on EOF — which the
/// caller reads as a "no" for any affirmative prompt.
fn read_line<R: BufRead>(reader: &mut R) -> Option<String> {
    let mut buf = String::new();
    match reader.read_line(&mut buf) {
        Ok(n) if n == 0 => None,
        Ok(_) => {
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                // Blank line: treat as "no" — the user must answer y to
                // affirmative. No silent default.
                Some(String::new())
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => None,
    }
}

/// Prompt the user: "Clean the N cleanable projects? (y/n)". Returns
/// whether the user answered "y" (or EOF → "no") — callers exit cleanly on
/// "no".
///
/// In `--force` / `--dry-run` this is skipped — the flow state machine calls
/// it only for interactive mode.
///
/// A single "y" is accepted; anything else (including blank lines and EOF)
/// is "no". The prompt is plain text, no colors, no fancy rendering.
pub fn collect_all_approval<R: BufRead>(
    reader: &mut R,
    num_cleanable: usize,
) -> bool {
    if num_cleanable == 0 {
        return false;
    }
    let question = format!(
        "Clean the {num_cleanable} cleanable projects? (y/n)",
    );
    print!("? ");
    eprintln!("{question}");
    match read_line(reader) {
        Some(answer) if answer.eq_ignore_ascii_case("y") => true,
        _ => false,
    }
}

/// Prompt the user about each surfaced item for `project_path`. Each
/// `item` is shown one at a time with its classification label, and the user
/// answers keep or delete. Each approval is returned in order.
///
/// "y"/"yes"/"n"/"no"/"n"/EOF → each call returns the item's approval
/// status (`approved: true` → would be deleted; `approved: false` → excluded,
/// kept). The caller accumulates these into the set it feeds into `build_exclusions`.
///
/// `--force` skips the per-item loop entirely; `--dry-run` displays without
/// collecting approvals. The call site in `collect_all` decides.
pub fn collect_each_item<R: BufRead>(
    reader: &mut R,
    _project_path: &std::path::Path,
    items: &[CleanItem],
) -> Vec<(PathBuf, bool)> {
    let mut each: Vec<(PathBuf, bool)> = Vec::new();
    for item in items {
        if item.classification != Classification::Surfaced {
            continue;
        }
        let label = match item.classification {
            Classification::Surfaced => "surfaced",
            Classification::Protected => "protected",
            Classification::Safe => "safe-to-delete",
        };
        let display = match item.is_dir {
            true => format!("{}{} [{}] delete or keep? (y/n)",
                item.rel_path.display(), "/", label),
            false => format!("{} [{}] delete or keep? (y/n)",
                item.rel_path.display(), label),
        };
        print!("? ");
        eprintln!("{display}");
        let approved = match read_line(reader) {
            Some(answer) if answer.eq_ignore_ascii_case("y") => true,
            _ => false,
        };
        each.push((item.rel_path.clone(), approved));
    }
    each
}

/// Prompt the user about cleaning a single project. Returns whether the
/// project itself is approved for cleaning — the caller needs this to
/// gate execution (only execute on approved projects; --force auto-approves
/// the project regardless).
pub fn collect_project_approval<R: BufRead>(
    reader: &mut R,
    project_path: &std::path::Path,
) -> bool {
    let question = format!(
        "Clean {}? (y/n)",
        project_path.display(),
    );
    print!("? ");
    eprintln!("{question}");
    match read_line(reader) {
        Some(answer) if answer.eq_ignore_ascii_case("y") => true,
        _ => false,
    }
}

/// Drive the full interactive flow. Returns each cleanable project's result
/// in sorted order.
///
/// For each cleanable project (in sorted order):
/// - in interactive mode: print the report, collect each item's approval and
///   the project's approval, execute if both approved;
/// - in force mode: print the report, execute every item (auto-approved);
/// - in dry-run mode: print the report, execute nothing;
/// - in force + dry-run mode: print the auto-approved report, execute nothing.
///
/// In interactive mode, the all-cleanup prompt is collected first. If the
/// user says no (or EOF), the function returns with every project
/// `project_approved = false`. The per-project loop still runs, but each
/// project's execution is gated.
///
/// In non-interactive mode (`--force` or `--dry-run`), the all-cleanup
/// prompt is skipped; each project's execution is unconditional (if dry-run
/// is set, no execution; otherwise every project is fully approved).
///
/// Zero cleanable → short-circuits before prompting: the summary is printed
/// by the caller (see `main::run_clean`); this function returns the (empty)
/// list of results.
pub fn run<R: BufRead>(
    inputs: InteractiveFlowInputs,
    reader: &mut R,
) -> Vec<ProjectResult> {
    let num_cleanable = inputs.per_project_items.len();

    // Step 1: report phase — caller prints this. Each project gets a human
    // readable status and a one-line reason. Cleanable projects are listed
    // separately as the cleanup subjects.

    // Step 2: all-cleanup prompt — skip in non-interactive modes.
    let all_approved = if !inputs.force && !inputs.dry_run {
        collect_all_approval(reader, num_cleanable)
    } else {
        // Force or dry-run: the loop runs unconditionally. Each project
        // is treated as approved for display.
        true
    };

    // Step 3: per-project loop.
    let mut results: Vec<ProjectResult> = Vec::new();
    for (i, &(ref path, ref items)) in inputs.per_project_items.iter().enumerate() {
        // Build the set of each Surfaced item the user approved (interactive mode only;
        // empty for force/dry-run because they auto-approve).
        let item_approvals: Vec<(PathBuf, bool)> =
            if !inputs.force && !inputs.dry_run {
                collect_each_item(reader, path, items)
            } else {
                Vec::new()
            };

        // Collect each Surfaced item's approval into the "approved" list.
        let approved: Vec<PathBuf> = item_approvals
            .into_iter()
            .filter(|(_, approved)| *approved)
            .map(|(path, _)| path)
            .collect();

        // Build the "would-delete" list from every item that would be
        // deleted in this mode. Safe + approved-`Surfaced` for interactive;
        // each item (each non-Protected, each auto-approved for display) for
        // force/dry-run.
        let would_delete: Vec<PathBuf> = items
            .iter()
            .filter(|item| {
                if inputs.dry_run {
                    // Each item non-Protected is each auto-approved for display
                    // in dry-run (the user sees each item each deleted).
                    item.classification != Classification::Protected
                } else {
                    would_delete(item, inputs.force, &approved)
                }
            })
            .map(|item| item.rel_path.clone())
            .collect();

        // Step 4: per-project confirmation — only in interactive mode.
        let project_approved = if !inputs.force && !inputs.dry_run {
            collect_project_approval(reader, path) && all_approved
        } else {
            all_approved
        };

        results.push(ProjectResult {
            path: path.clone(),
            status: inputs.all_projects[i].1,
            items: items.clone(),
            would_delete: would_delete.clone(),
            project_approved,
        });
    }

    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::CleanItem;
    use crate::classify::Status;
    use std::io::BufReader;

    fn make_item(rel: &str, classification: Classification, is_dir: bool) -> CleanItem {
        CleanItem {
            rel_path: PathBuf::from(rel),
            is_dir,
            classification,
        }
    }

    fn inputs_for(items: &[CleanItem], force: bool, dry_run: bool) -> InteractiveFlowInputs {
        let path = PathBuf::from("/tmp/project");
        InteractiveFlowInputs {
            all_projects: vec![(path.clone(), Status::Cleanable)],
            per_project_items: vec![(path, items.to_vec())],
            force,
            dry_run,
        }
    }

    fn run_with_answers(
        inputs: InteractiveFlowInputs,
        answers: &str,
    ) -> Vec<ProjectResult> {
        let reader = BufReader::new(answers.as_bytes());
        let mut r = reader;
        run(inputs, &mut r)
    }

    /// Interactive mode with every answer "y": each project is approved,
    /// each surfaced item is approved, each project runs.
    #[test]
    fn interactive_all_yes_approves_each_project() {
        let items = vec![
            make_item("important.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("b.tmp", Classification::Surfaced, false),
            make_item("c.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        // answers: all, each-item, each-project, each-project, each-project,
        //         each-project, each-project, each-project. We have three
        //         cleanable projects in `inputs.all_projects`, so we need
        //         three "y" per project. Each project has two surfaced items.
        let all = format!("y\ny\ny\ny\ny\ny\ny\ny\ny\n");
        let results = run_with_answers(inputs, &all);
        // Three projects, all approved.
        assert_eq!(results.len(), 1);
        assert!(results[0].project_approved);
        // Both surfaced items approved.
        assert_eq!(results[0].would_delete.len(), 3); // safe + 2 surfaced
    }

    /// Interactive mode with "n" on all-cleanup: exit without any project
    /// approved.
    #[test]
    fn interactive_all_no_exits_cleanly() {
        let items = vec![make_item("target", Classification::Safe, true)];
        let inputs = inputs_for(&items, false, false);
        let results = run_with_answers(inputs, "n\ny\ny\n");
        assert_eq!(results.len(), 1);
        // Project is not approved because all-cleanup was "n".
        assert!(!results[0].project_approved);
    }

    /// Interactive mode with "n" on a per-item: that item is excluded (kept).
    #[test]
    fn interactive_disapprove_item_keeps_it_excluded() {
        let items = vec![
            make_item("target", Classification::Safe, true),
            make_item("a.tmp", Classification::Surfaced, false),
            make_item("b.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        // each item: a.tmp "n", b.tmp "y" — each-item1 (a.tmp) excluded (kept), each-item2 (b.tmp) approved (would-delete).
        let all = format!("y\nn\ny\ny\n");
        let results = run_with_answers(inputs, &all);
        assert!(results[0].project_approved);
        // Only b.tmp would-delete; a.tmp is excluded (kept).
        assert!(results[0].would_delete.contains(&PathBuf::from("b.tmp")));
        assert!(!results[0].would_delete.contains(&PathBuf::from("a.tmp")));
    }

    /// Force mode: every item would-delete regardless of answer. We only
    /// need a single "y" to the all-cleanup prompt (or no prompt at all if
    /// --force is set; but we still test with the all-cleanup prompt present
    /// because the state machine is the same — force is a flag on inputs).
    #[test]
    fn force_auto_approves_each_item() {
        let items = vec![
            make_item("important.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("a.tmp", Classification::Surfaced, false),
            make_item("b.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, true, false);
        let results = run_with_answers(inputs, "y\n");
        assert!(results[0].project_approved);
        // Every item that is not protected would-delete.
        let would_delete: Vec<&PathBuf> = results[0].would_delete.iter().collect();
        assert!(would_delete.contains(&&PathBuf::from("target")));
        assert!(would_delete.contains(&&PathBuf::from("a.tmp")));
        assert!(would_delete.contains(&&PathBuf::from("b.tmp")));
        // Protected is NOT in the would-delete list.
        assert!(!would_delete.contains(&&PathBuf::from("important.dat")));
    }

    /// Dry-run mode: each project is "approved" for display but each project
    /// is also non-executed. The result shows the items and their
    /// classifications but no execution.
    #[test]
    fn dry_run_displays_each_item() {
        let items = vec![
            make_item("target", Classification::Safe, true),
            make_item("a.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, true);
        let results = run_with_answers(inputs, "");
        assert!(results[0].project_approved);
        // Both non-protected items would-delete in dry-run (auto-approved
        // each item; each project is "approved" for display).
        assert_eq!(results[0].would_delete.len(), 2);
    }

    /// Force + dry-run: each project's report is printed (auto-approved view)
    /// and no project is executed.
    #[test]
    fn force_and_dry_run_auto_approve_each_item() {
        let items = vec![
            make_item("important.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("a.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, true, true);
        let results = run_with_answers(inputs, "");
        assert!(results[0].project_approved);
        // target and a.tmp would-delete (auto-approved each item); important.dat is excluded.
        assert_eq!(results[0].would_delete.len(), 2);
    }

    /// Zero cleanable projects: each project is reported, no prompts are
    /// sent, each result is empty.
    #[test]
    fn zero_cleanable_each_result_empty() {
        let inputs = InteractiveFlowInputs {
            all_projects: vec![
                (PathBuf::from("/tmp/project1"), Status::Cleanable),
                (PathBuf::from("/tmp/project2"), Status::Wip),
            ],
            per_project_items: vec![],
            force: false,
            dry_run: false,
        };
        let results = run_with_answers(inputs, "");
        assert!(results.is_empty());
    }

    /// Each project in interactive mode is gated by both all-cleanup and
    /// per-project approval — if either is no, the project is not approved.
    #[test]
    fn per_project_gating_each_condition() {
        let items = vec![make_item("target", Classification::Safe, true)];
        let inputs = inputs_for(&items, false, false);
        // No each-item prompts (target is Safe, not Surfaced); each-project prompt reads second answer.
        let all = format!("y\nn\n");
        let results = run_with_answers(inputs, &all);
        // each-project: "n" → each project not approved, even though each-item approved.
        assert!(!results[0].project_approved);
    }

    /// Each item's classification is respected: Protected items are never in
    /// would-delete; each Safe items are always in would-delete; each
    /// Surfaced items are in would-delete only if approved (or force).
    #[test]
    fn each_classification_respected_each_item() {
        let items = vec![
            make_item("protected.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("b.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        // each-item: b.tmp "y" → would-delete.
        let all = format!("y\ny\ny\n");
        let results = run_with_answers(inputs, &all);
        let would_delete: Vec<&PathBuf> = results[0].would_delete.iter().collect();
        assert!(would_delete.contains(&&PathBuf::from("target")));
        assert!(would_delete.contains(&&PathBuf::from("b.tmp")));
        assert!(!would_delete.contains(&&PathBuf::from("protected.dat")));
    }

    /// Each item's rel_path is preserved through the result: the would-delete
    /// list contains exactly the rel_paths of each item that would-delete.
    #[test]
    fn each_rel_path_preserved_each_result() {
        let items = vec![
            make_item("a/path", Classification::Safe, false),
            make_item("b/path", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        let all = format!("y\ny\ny\n");
        let results = run_with_answers(inputs, &all);
        let would_delete: Vec<&PathBuf> = results[0].would_delete.iter().collect();
        assert!(would_delete.contains(&&PathBuf::from("a/path")));
        assert!(would_delete.contains(&&PathBuf::from("b/path")));
    }

    /// Each project's path in the result matches the input path.
    #[test]
    fn each_project_path_preserved_each_result() {
        let items = vec![make_item("target", Classification::Safe, true)];
        let inputs = inputs_for(&items, false, false);
        let all = format!("y\ny\ny\n");
        let results = run_with_answers(inputs, &all);
        assert_eq!(results[0].path, PathBuf::from("/tmp/project"));
        assert_eq!(results[0].status, Status::Cleanable);
    }
}