/// Per-file result for console output (re-exported from plan_builder).
use super::plan_builder::FileResult;

/// Print per-file apply results in the format specified by the spec.
///
/// Format:
/// ```text
///   ✓ ~/.gitconfig (base)
///   ✓ ~/.config/nvim/plugins.lua (macbook ← overrides base)
///   ────────────────────────────────────────
///   3 applied, 1 override, 2 skipped (unchanged)
/// ```
pub(crate) fn print_per_file_summary(
    file_results: &[FileResult],
    orphans: &[(String, String)],
    dry_run: bool,
) {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    let check = crate::symbols::check();

    // Print orphan removals first
    if !orphans.is_empty() {
        for (_repo_relative_path, target_rel) in orphans {
            println!("  {}{} orphan removed", prefix, target_rel);
        }
    }

    // Sort results by target path for consistent output
    let mut sorted = file_results.to_vec();
    sorted.sort_by(|a, b| a.target.cmp(&b.target));

    let mut applied_count = 0;
    let mut override_count = 0;
    let mut skipped_count = 0;

    for result in &sorted {
        let target_str = crate::paths::format_target_display(&result.target);

        if result.skipped {
            skipped_count += 1;
            continue;
        }

        if result.applied {
            applied_count += 1;
        }

        println!("  {}{} {} ({})", prefix, check, target_str, result.tier);

        if let Some(ref lower_tier) = result.overrides {
            override_count += 1;
            println!("  {}  (overrides {})", prefix, lower_tier);
        }
    }

    let separator = "────────────────────────────────────────";
    println!("  {}{}", prefix, separator);

    if dry_run {
        println!(
            "  {}{} would be applied, {} override, {} skipped (unchanged)",
            prefix, applied_count, override_count, skipped_count
        );
        println!("  {}no changes made", prefix);
    } else {
        println!(
            "  {} applied, {} override, {} skipped (unchanged)",
            applied_count, override_count, skipped_count
        );
    }
}
