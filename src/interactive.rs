//! Interactive cleaning flow for offcut (issue #8).
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
//! | interactive     | yes → y/n         | yes → delete/keep | yes → clean?      | yes if approved
//! | --force         | no                | no               | no                 | yes
//! | --dry-run       | no                | no               | no                 | no
//! | --force --dry-run | no              | no               | no                 | no (show)
//!
//! ## Decision state machine
//!
//! 1. **Report phase** — show every project sorted by status, report each
//!    non-cleanable one with a one-line reason, list each cleanable one.
//! 2. **All-cleanup prompt** — "Clean the N cleanable projects? (y/n)". If
//!    no (or EOF / --force / --dry-run), exit without touching any project:
//!    no further prompts are shown and every result comes back unapproved.
//! 3. **Per-project loop** (in sorted order, each cleanable):
//!    a. enumerate untracked items (dry-run);
//!    b. print the project path and the list of each item that would be
//!    deleted;
//!    c. for each `Surfaced` item: prompt keep or delete; record approval;
//!    d. prompt per-project confirmation ("Clean <path>? (y/n)"); record
//!    approval;
//!    e. if approved (or --force): execute `git clean -xfd -e <globs>`;
//!    if --dry-run: print the report only.
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
//! In `--dry-run` without `--force`, only `Safe` items are reported as
//! would-delete; `Surfaced` items are what a real interactive run would
//! prompt about, so the preview must not claim they would be deleted.
//! `--force --dry-run` previews the force run, so every non-`Protected`
//! item is reported as would-delete.
//!
//! The `clean` function in `clean.rs` owns the deletion step; this module
//! owns the approval collection.

use std::io::BufRead;
use std::path::PathBuf;

use crate::classify::Status;
use crate::clean::{Classification, CleanItem};
use crate::output;

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
    /// The items that would be deleted for this project (Safe + approved
    /// Surfaced in interactive mode; every non-Protected item under
    /// --force; Safe only under plain --dry-run, which previews the
    /// interactive run without pretending Surfaced items were approved).
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
        Ok(0) => None,
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

/// Prompt the user: "Remove gitignored paths from the N cleanable projects? [y/N]". Returns
/// whether the user answered "y" (or EOF → "no") — callers exit cleanly on
/// "no".
///
/// In `--force` / `--dry-run` this is skipped — the flow state machine calls
/// it only for interactive mode.
///
/// A single "y" is accepted; anything else (including blank lines and EOF)
/// is "no". The prompt is plain text, no colors, no fancy rendering.
pub fn collect_all_approval<R: BufRead>(reader: &mut R, num_cleanable: usize) -> bool {
    if num_cleanable == 0 {
        return false;
    }
    let hint = if output::stderr_terminal_ui_enabled() {
        "[y/N]"
    } else {
        "(y/n)"
    };
    let question =
        format!("Remove gitignored paths from the {num_cleanable} cleanable projects? {hint}");
    eprintln!("? {question}");
    matches!(read_line(reader), Some(answer) if answer.eq_ignore_ascii_case("y"))
}

/// Print the pre-approval report for one project: its path and every
/// enumerated item with its classification and fate. Goes to stderr — the
/// same stream as the prompts — so the user sees exactly what they are
/// about to approve, in order, before any question is asked.
fn print_project_report(path: &std::path::Path, items: &[CleanItem]) {
    eprintln!("{}:", path.display());
    eprintln!("  Gitignored review");
    for item in items {
        let (label, fate) = match item.classification {
            Classification::Protected => ("protected", "kept"),
            Classification::Safe => ("safe-to-delete", "will delete"),
            Classification::Surfaced => ("surfaced", "needs approval"),
        };
        eprintln!(
            "  {}{} [{label}] ({fate})",
            item.rel_path.display(),
            if item.is_dir { "/" } else { "" },
        );
    }
}

/// Prompt the user about each surfaced item for `project_path`. Each
/// `Surfaced` item is shown one at a time and the user answers delete or
/// keep. The approvals are returned in item order.
///
/// Only a (case-insensitive) "y" approves; anything else — including
/// "yes", blank lines, and EOF — keeps the item (`approved: false` →
/// excluded). The caller accumulates the approvals into the set it feeds
/// into `build_exclusions`.
///
/// `--force` skips the per-item loop entirely; `--dry-run` displays without
/// collecting approvals. The call site in `run` decides.
pub fn collect_each_item<R: BufRead>(
    reader: &mut R,
    _project_path: &std::path::Path,
    items: &[CleanItem],
) -> Vec<(PathBuf, bool)> {
    let mut approvals: Vec<(PathBuf, bool)> = Vec::new();
    for item in items {
        if item.classification != Classification::Surfaced {
            continue;
        }
        eprintln!(
            "? {}{} [surfaced] delete? {}",
            item.rel_path.display(),
            if item.is_dir { "/" } else { "" },
            if output::stderr_terminal_ui_enabled() {
                "[y/N]"
            } else {
                "(y/n)"
            },
        );
        let approved =
            matches!(read_line(reader), Some(answer) if answer.eq_ignore_ascii_case("y"));
        approvals.push((item.rel_path.clone(), approved));
    }
    approvals
}

/// Prompt the user about cleaning a single project. Returns whether the
/// project itself is approved for cleaning — the caller needs this to
/// gate execution (only execute on approved projects; --force auto-approves
/// the project regardless).
pub fn collect_project_approval<R: BufRead>(
    reader: &mut R,
    project_path: &std::path::Path,
) -> bool {
    let hint = if output::stderr_terminal_ui_enabled() {
        "[y/N]"
    } else {
        "(y/n)"
    };
    let question = format!(
        "Remove these gitignored paths from {}? {hint}",
        project_path.display(),
    );
    eprintln!("? {question}");
    matches!(read_line(reader), Some(answer) if answer.eq_ignore_ascii_case("y"))
}

/// Drive the full interactive flow. Returns each cleanable project's result
/// in sorted order (one result per `per_project_items` entry, same order).
///
/// For each cleanable project (in sorted order):
/// - in interactive mode: print the project's item report, collect each
///   surfaced item's approval and the project's approval — the report is
///   shown *before* any prompt so nothing is approved sight-unseen;
/// - in force mode: no prompts, every item auto-approved;
/// - in dry-run mode: no prompts, Safe items marked would-delete;
/// - in force + dry-run mode: no prompts, every non-Protected item marked
///   would-delete (a preview of the force run).
///
/// In interactive mode, the all-cleanup prompt is collected first. If the
/// user says no (or EOF), no further prompts are shown and the function
/// returns with every project `project_approved = false`.
///
/// In non-interactive mode (`--force` or `--dry-run`), the all-cleanup
/// prompt is skipped and every project comes back approved (the caller
/// gates actual execution on `dry_run`).
///
/// Zero cleanable → short-circuits before prompting: the summary is printed
/// by the caller (see `main::run_clean`); this function returns the (empty)
/// list of results.
pub fn run<R: BufRead>(inputs: InteractiveFlowInputs, reader: &mut R) -> Vec<ProjectResult> {
    let num_cleanable = inputs.per_project_items.len();
    let interactive = !inputs.force && !inputs.dry_run;

    // Step 1: report phase — caller prints this. Each project gets a human
    // readable status and a one-line reason. Cleanable projects are listed
    // separately as the cleanup subjects.

    // Step 2: all-cleanup prompt — skip in non-interactive modes.
    let all_approved = if interactive {
        collect_all_approval(reader, num_cleanable)
    } else {
        true
    };

    // Step 3: per-project loop.
    let mut results: Vec<ProjectResult> = Vec::new();
    for (path, items) in &inputs.per_project_items {
        // Show what would be deleted, then ask. A declined all-cleanup
        // prompt suppresses every later prompt: the user already said no.
        let item_approvals: Vec<(PathBuf, bool)> = if interactive && all_approved {
            print_project_report(path, items);
            collect_each_item(reader, path, items)
        } else {
            Vec::new()
        };

        let approved: Vec<PathBuf> = item_approvals
            .into_iter()
            .filter(|(_, approved)| *approved)
            .map(|(path, _)| path)
            .collect();

        let would_delete_paths: Vec<PathBuf> = items
            .iter()
            .filter(|item| {
                if inputs.dry_run && !inputs.force {
                    // Plain dry-run previews the interactive run: Safe items
                    // would be deleted, Surfaced items would be prompted
                    // about, so they must not be claimed as deletions.
                    item.classification == Classification::Safe
                } else {
                    would_delete(item, inputs.force, &approved)
                }
            })
            .map(|item| item.rel_path.clone())
            .collect();

        // Step 4: per-project confirmation — only in interactive mode, and
        // only when the all-cleanup prompt was approved.
        let project_approved = if interactive {
            all_approved && collect_project_approval(reader, path)
        } else {
            all_approved
        };

        // `per_project_items` holds only cleanable projects, a subset of
        // `all_projects`, so the status must be looked up by path — the
        // loop index does not line up with the full sorted list.
        let status = inputs
            .all_projects
            .iter()
            .find(|(p, _)| p == path)
            .map(|&(_, s)| s)
            .unwrap_or(Status::Cleanable);

        results.push(ProjectResult {
            path: path.clone(),
            status,
            items: items.clone(),
            would_delete: would_delete_paths,
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
    use crate::classify::Status;
    use crate::clean::CleanItem;
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

    fn run_with_answers(inputs: InteractiveFlowInputs, answers: &str) -> Vec<ProjectResult> {
        let reader = BufReader::new(answers.as_bytes());
        let mut r = reader;
        run(inputs, &mut r)
    }

    /// Interactive mode with every answer "y": the project is approved and
    /// every surfaced item is approved.
    #[test]
    fn interactive_all_yes_approves_each_project() {
        let items = vec![
            make_item("important.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("b.tmp", Classification::Surfaced, false),
            make_item("c.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        // One cleanable project consuming four answers: the all-cleanup
        // prompt, one per surfaced item (b.tmp, c.tmp), and the per-project
        // confirmation.
        let all = "y\ny\ny\ny\n".to_string();
        let results = run_with_answers(inputs, &all);
        assert_eq!(results.len(), 1);
        assert!(results[0].project_approved);
        assert_eq!(results[0].would_delete.len(), 3); // safe + 2 surfaced
    }

    /// Interactive mode with "n" on all-cleanup: no further answers are
    /// consumed and no project is approved.
    #[test]
    fn interactive_all_no_exits_cleanly() {
        let items = vec![make_item("target", Classification::Safe, true)];
        let inputs = inputs_for(&items, false, false);
        let results = run_with_answers(inputs, "n\n");
        assert_eq!(results.len(), 1);
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
        // Answers: all-cleanup "y", a.tmp "n" (kept), b.tmp "y" (delete),
        // per-project "y".
        let all = "y\nn\ny\ny\n".to_string();
        let results = run_with_answers(inputs, &all);
        assert!(results[0].project_approved);
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

    /// Dry-run mode: no prompts, nothing executed. Only Safe items are
    /// reported as would-delete — a Surfaced item would be prompted about
    /// in a real interactive run, so the preview must not claim it.
    #[test]
    fn dry_run_displays_each_item() {
        let items = vec![
            make_item("target", Classification::Safe, true),
            make_item("a.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, true);
        let results = run_with_answers(inputs, "");
        assert!(results[0].project_approved);
        assert_eq!(results[0].would_delete, vec![PathBuf::from("target")]);
    }

    /// Force + dry-run previews the force run: every non-Protected item is
    /// reported as would-delete and nothing is executed.
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
        // target and a.tmp would-delete; important.dat is Protected.
        assert_eq!(results[0].would_delete.len(), 2);
    }

    /// Zero cleanable projects: no prompts are sent and the result list is
    /// empty.
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

    /// A project in interactive mode is gated by both the all-cleanup and
    /// the per-project approval — if either is no, it is not approved.
    #[test]
    fn per_project_gating_each_condition() {
        let items = vec![make_item("target", Classification::Safe, true)];
        let inputs = inputs_for(&items, false, false);
        // No per-item prompts (target is Safe, not Surfaced); the
        // per-project prompt reads the second answer.
        let all = "y\nn\n".to_string();
        let results = run_with_answers(inputs, &all);
        // Per-project answer "n" → not approved despite all-cleanup "y".
        assert!(!results[0].project_approved);
    }

    /// Classification is respected: Protected items are never in
    /// would-delete; Safe items always are; Surfaced items are in
    /// would-delete only when approved (or under force).
    #[test]
    fn each_classification_respected_each_item() {
        let items = vec![
            make_item("protected.dat", Classification::Protected, false),
            make_item("target", Classification::Safe, true),
            make_item("b.tmp", Classification::Surfaced, false),
        ];
        let inputs = inputs_for(&items, false, false);
        // Answers: all-cleanup "y", b.tmp "y", per-project "y".
        let all = "y\ny\ny\n".to_string();
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
        let all = "y\ny\ny\n".to_string();
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
        let all = "y\ny\ny\n".to_string();
        let results = run_with_answers(inputs, &all);
        assert_eq!(results[0].path, PathBuf::from("/tmp/project"));
        assert_eq!(results[0].status, Status::Cleanable);
    }

    /// The result status is looked up by path, not by index: non-cleanable
    /// projects sort before cleanable ones in `all_projects`, so indexing
    /// that list with the cleanable-only loop index would mislabel the
    /// cleanable project (e.g. as no-git).
    #[test]
    fn status_looked_up_by_path_with_non_cleanable_projects_present() {
        let cleanable = PathBuf::from("/tmp/zz-cleanable");
        let inputs = InteractiveFlowInputs {
            all_projects: vec![
                (PathBuf::from("/tmp/aa-no-git"), Status::NoGit),
                (cleanable.clone(), Status::Cleanable),
            ],
            per_project_items: vec![(
                cleanable.clone(),
                vec![make_item("target", Classification::Safe, true)],
            )],
            force: false,
            dry_run: true,
        };
        let results = run_with_answers(inputs, "");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, cleanable);
        assert_eq!(results[0].status, Status::Cleanable);
    }
}
