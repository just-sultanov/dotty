/// Per-file result for console output (re-exported from plan_builder).
use super::plan_builder::{DirResult, FileResult};

/// Print apply summary: per-file override annotations + `done` (normal mode)
/// + counts.
///
/// Layout (normal mode, with overrides):
/// ```text
///   <tier> - <target>                              [Overrides: block, optional]
///   done                                           [only if had_actions, not dry-run]
///   N applied, M override, K skipped (unchanged)   [always]
/// ```
///
/// The executor prints action lines itself (with `✓ <action>` in normal
/// mode, or `[dry-run] <action>` in dry-run). A blank line is inserted
/// before the summary sections whenever the executor actually printed
/// something (`had_actions`), so the summary reads as a clear postlude.
///
/// Per-file override lines are printed ONLY when there are overrides —
/// for the common case (no overrides), the user sees just `done` (or
/// nothing in dry-run) and the counts. Orphan annotations are already
/// emitted by the executor and aggregated in counts.
///
/// Directory symlink results (applied dir-symlinks) are shown in a
/// "Directory symlinks:" section before overrides.
pub(crate) fn print_per_file_summary(
    file_results: &[FileResult],
    dir_results: &[DirResult],
    orphans: &[(String, String)],
    dry_run: bool,
    had_actions: bool,
) {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    let check = crate::symbols::check();

    // Per-file override lines: only the winning tier + target.
    let mut override_lines: Vec<String> = Vec::new();
    for result in file_results {
        if result.overrides.is_some() {
            let target_str = crate::paths::format_target_display(&result.target);
            if dry_run {
                override_lines.push(format!("{prefix}{} - {}", result.tier, target_str));
            } else {
                override_lines.push(format!("{check} {} - {}", result.tier, target_str));
            }
        }
    }

    // Directory symlink lines: applied dir-symlinks with tier.
    let dir_applied: Vec<&DirResult> = dir_results.iter().filter(|r| r.applied).collect();
    let mut dir_lines: Vec<String> = Vec::new();
    for dr in &dir_applied {
        let target_str = crate::paths::format_target_display(&dr.target_dir);
        if dry_run {
            dir_lines.push(format!("{prefix}{} - {}/", dr.tier, target_str));
        } else {
            dir_lines.push(format!("{check} {} - {}/", dr.tier, target_str));
        }
    }

    // Blank line between executor output and the first summary section.
    if had_actions {
        println!();
    }

    // Directory symlinks section
    if !dir_lines.is_empty() {
        println!("Directory symlinks:");
        for line in &dir_lines {
            println!("{line}");
        }
        println!();
    }

    if !override_lines.is_empty() {
        println!("Overrides:");
        for line in &override_lines {
            println!("{line}");
        }
        println!();
    }

    // `done` — confirmation line.
    if !dry_run && had_actions {
        println!("done");
    }

    // Counts: include both file and dir results.
    let mut applied_count = 0;
    let mut override_count = 0;
    let mut skipped_count = 0;
    for result in file_results {
        if result.skipped {
            skipped_count += 1;
        } else if result.applied {
            applied_count += 1;
        }
        if result.overrides.is_some() {
            override_count += 1;
        }
    }
    // Add dir results to counts (dirs don't have overrides)
    for result in dir_results {
        if result.skipped {
            skipped_count += 1;
        } else if result.applied {
            applied_count += 1;
        }
    }
    let orphan_count = orphans.len();

    if dry_run {
        if orphan_count > 0 {
            println!(
                "{applied_count} would be applied, {override_count} override, {skipped_count} skipped (unchanged), {orphan_count} orphan removed"
            );
        } else {
            println!(
                "{applied_count} would be applied, {override_count} override, {skipped_count} skipped (unchanged)"
            );
        }
    } else if orphan_count > 0 {
        println!(
            "{applied_count} applied, {override_count} override, {skipped_count} skipped (unchanged), {orphan_count} orphan removed"
        );
    } else {
        println!(
            "{applied_count} applied, {override_count} override, {skipped_count} skipped (unchanged)"
        );
    }
}
