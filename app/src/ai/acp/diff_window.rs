use ai::diff_validation::{DiffDelta, DiffType};
use similar::{DiffTag, TextDiff};

/// Returns `(old, new)` trimmed to changed hunks plus `context` lines on each side.
pub fn window_diff(old: &str, new: &str, context: usize) -> (String, String) {
    if old.is_empty() {
        return (String::new(), new.to_string());
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut include_old = vec![false; old_lines.len()];
    let mut include_new = vec![false; new_lines.len()];

    let diff = TextDiff::from_lines(old, new);
    let mut old_i = 0usize;
    let mut new_i = 0usize;
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {
                old_i += op.old_range().len();
                new_i += op.new_range().len();
            }
            DiffTag::Delete => {
                for i in old_i..old_i + op.old_range().len() {
                    include_old[i] = true;
                }
                old_i += op.old_range().len();
            }
            DiffTag::Insert => {
                for i in new_i..new_i + op.new_range().len() {
                    include_new[i] = true;
                }
                // Anchor old-side context at the insertion point.
                if old_i < include_old.len() {
                    include_old[old_i] = true;
                } else {
                    let last = include_old.len().saturating_sub(1);
                    if last < include_old.len() {
                        include_old[last] = true;
                    }
                }
                new_i += op.new_range().len();
            }
            DiffTag::Replace => {
                for i in old_i..old_i + op.old_range().len() {
                    include_old[i] = true;
                }
                for i in new_i..new_i + op.new_range().len() {
                    include_new[i] = true;
                }
                old_i += op.old_range().len();
                new_i += op.new_range().len();
            }
        }
    }

    expand_with_context(&mut include_old, context);
    expand_with_context(&mut include_new, context);

    let windowed_old = join_selected_lines(&old_lines, &include_old);
    let windowed_new = join_selected_lines(&new_lines, &include_new);
    (windowed_old, windowed_new)
}

/// Builds a [`FileDiff`] from full old/new file contents, windowing hunks for display.
pub fn file_diff_from_old_new(
    old_full: &str,
    new_full: &str,
    file_path: String,
    context: usize,
) -> crate::ai::blocklist::diff_types::FileDiff {
    use crate::ai::blocklist::diff_types::FileDiff;

    const MAX_LINES_WITHOUT_WINDOWING: usize = 64;

    if old_full.lines().count() <= MAX_LINES_WITHOUT_WINDOWING
        && new_full.lines().count() <= MAX_LINES_WITHOUT_WINDOWING
    {
        return FileDiff::new(
            old_full.to_string(),
            file_path,
            DiffType::update(line_diff_deltas(old_full, new_full), None),
        );
    }

    if old_full.is_empty() {
        let (_, windowed_new) = window_diff("", new_full, context);
        return FileDiff::new(String::new(), file_path, DiffType::creation(windowed_new));
    }

    let (windowed_old, windowed_new) = window_diff(old_full, new_full, context);
    let deltas = line_diff_deltas(&windowed_old, &windowed_new);
    FileDiff::new(
        windowed_old,
        file_path,
        DiffType::update(deltas, None),
    )
}

/// Converts a line diff between `base` and `target` into editor [`DiffDelta`]s (1-indexed).
fn line_diff_deltas(base: &str, target: &str) -> Vec<DiffDelta> {
    if base == target {
        return Vec::new();
    }

    let target_lines: Vec<&str> = target.lines().collect();
    let diff = TextDiff::from_lines(base, target);
    let mut deltas = Vec::new();
    let mut old_line = 1usize;

    for op in diff.ops() {
        let old_len = op.old_range().len();
        let new_range = op.new_range();
        let insertion = if new_range.is_empty() {
            String::new()
        } else {
            target_lines[new_range.start..new_range.end].join("\n")
        };

        match op.tag() {
            DiffTag::Equal => {
                old_line += old_len;
            }
            DiffTag::Delete => {
                deltas.push(DiffDelta {
                    replacement_line_range: old_line..old_line + old_len,
                    insertion: String::new(),
                });
                old_line += old_len;
            }
            DiffTag::Insert => {
                deltas.push(DiffDelta {
                    replacement_line_range: old_line..old_line,
                    insertion,
                });
            }
            DiffTag::Replace => {
                deltas.push(DiffDelta {
                    replacement_line_range: old_line..old_line + old_len,
                    insertion,
                });
                old_line += old_len;
            }
        }
    }

    deltas
}

fn expand_with_context(flags: &mut [bool], context: usize) {
    let changed: Vec<usize> = flags
        .iter()
        .enumerate()
        .filter(|(_, included)| **included)
        .map(|(index, _)| index)
        .collect();
    for index in changed {
        let start = index.saturating_sub(context);
        let end = (index + context + 1).min(flags.len());
        for flag in &mut flags[start..end] {
            *flag = true;
        }
    }
}

fn join_selected_lines(lines: &[&str], include: &[bool]) -> String {
    lines
        .iter()
        .zip(include)
        .filter(|(_, included)| **included)
        .map(|(line, _)| *line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolves full old/new file contents for an ACP edit diff.
///
/// When the agent omits `old_text` and sends an incomplete `new_text` snapshot (missing
/// lines that already exist on disk), treat on-disk content as authoritative for the final
/// file and derive the pre-edit version from the actual insertion.
pub fn resolve_edit_old_new(
    old_text: Option<String>,
    new_text: String,
    on_disk: Option<String>,
) -> (String, String) {
    if let Some(old) = old_text {
        return (old, new_text);
    }

    let Some(disk) = on_disk else {
        return (String::new(), new_text);
    };

    if disk == new_text {
        return (disk, new_text);
    }

    if is_ordered_subsequence(&new_text, &disk) && disk.lines().count() > new_text.lines().count() {
        let new_full = disk;
        let old_full = derive_pre_edit_from_incomplete_acp_new(&new_full, &new_text);
        return (old_full, new_full);
    }

    (disk, new_text)
}

fn is_ordered_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack_lines = haystack.lines();
    'outer: for needle_line in needle.lines() {
        for haystack_line in haystack_lines.by_ref() {
            if haystack_line == needle_line {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

fn derive_pre_edit_from_incomplete_acp_new(disk: &str, acp_new: &str) -> String {
    let mut best: Option<(String, usize)> = None;

    for (index, _) in acp_new.lines().enumerate() {
        let candidate = acp_new
            .lines()
            .enumerate()
            .filter(|(line_index, _)| *line_index != index)
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n");
        if !is_insert_only_diff(&candidate, acp_new) {
            continue;
        }

        let merged = merge_disk_only_lines(&candidate, disk, acp_new);
        if !is_insert_only_diff(&merged, disk) {
            continue;
        }
        if merged.lines().next() != disk.lines().next() {
            continue;
        }

        let inserted_line_count = count_inserted_lines(&merged, disk);
        if best
            .as_ref()
            .is_none_or(|(_, count)| inserted_line_count < *count)
        {
            best = Some((merged, inserted_line_count));
        }
    }

    best.map(|(merged, _)| merged)
        .unwrap_or_else(|| acp_new.to_string())
}

fn count_inserted_lines(old: &str, new: &str) -> usize {
    let mut count = 0usize;
    for op in TextDiff::from_lines(old, new).ops() {
        if matches!(op.tag(), DiffTag::Insert) {
            count += op.new_range().len();
        }
    }
    count
}

/// Re-inserts lines that exist on disk but were omitted from the agent's `new_text` snapshot.
fn merge_disk_only_lines(pre_edit_acp: &str, disk: &str, acp_new: &str) -> String {
    let pre_lines: Vec<&str> = pre_edit_acp.lines().collect();
    let mut pre_i = 0usize;
    let mut merged = Vec::new();

    for disk_line in disk.lines() {
        if pre_i < pre_lines.len() && pre_lines[pre_i] == disk_line {
            merged.push(disk_line);
            pre_i += 1;
        } else if is_disk_only_line(disk_line, acp_new) {
            merged.push(disk_line);
        }
    }

    merged.join("\n")
}

fn is_disk_only_line(line: &str, acp_new: &str) -> bool {
    !acp_new.lines().any(|candidate| candidate == line)
}

fn is_insert_only_diff(old: &str, new: &str) -> bool {
    let diff = TextDiff::from_lines(old, new);
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal | DiffTag::Insert => {}
            DiffTag::Delete | DiffTag::Replace => return false,
        }
    }
    !old.is_empty() || !new.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_full_file_edit_to_changed_hunks() {
        let old = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut new_lines: Vec<String> = (1..=20).map(|line| format!("line {line}")).collect();
        new_lines[9] = "line 10 changed".to_string();
        new_lines[10] = "line 11 changed".to_string();
        let new = new_lines.join("\n");

        let (windowed_old, windowed_new) = window_diff(&old, &new, 2);

        assert!(windowed_old.contains("line 8"));
        assert!(windowed_old.contains("line 10"));
        assert!(windowed_old.contains("line 12"));
        assert!(!windowed_old.contains("line 1\n"));
        assert!(!windowed_old.contains("line 20"));
        assert!(windowed_new.contains("line 10 changed"));
        assert!(windowed_new.contains("line 11 changed"));
    }

    #[test]
    fn new_file_returns_empty_old() {
        let (old, new) = window_diff("", "hello\nworld", 5);
        assert!(old.is_empty());
        assert_eq!(new, "hello\nworld");
    }

    #[test]
    fn file_diff_from_old_new_uses_update_when_old_exists() {
        let old = "pub mod a;\npub mod b;";
        let new = "// test comment\npub mod a;\npub mod b;";
        let file_diff = file_diff_from_old_new(old, new, "mod.rs".to_string(), 2);
        assert!(matches!(file_diff.diff_type, DiffType::Update { .. }));
        assert!(!file_diff.base.content.is_empty());
    }

    #[test]
    fn single_line_prepend_produces_one_insert_delta() {
        let old = "pub mod a;\npub mod b;";
        let new = "// test comment\npub mod a;\npub mod b;";
        let deltas = line_diff_deltas(old, new);
        let inserted_lines: usize = deltas.iter().map(|d| d.insertion.lines().count()).sum();
        assert_eq!(inserted_lines, 1);
    }

    #[test]
    fn file_diff_from_old_new_counts_single_added_line() {
        let old = "pub mod a;\npub mod b;";
        let new = "// test comment\npub mod a;\npub mod b;";
        let file_diff = file_diff_from_old_new(old, new, "mod.rs".to_string(), 2);
        let DiffType::Update { deltas, .. } = file_diff.diff_type else {
            panic!("expected update diff");
        };
        let inserted_lines: usize = deltas.iter().map(|d| d.insertion.lines().count()).sum();
        assert_eq!(inserted_lines, 1);
    }

    #[test]
    fn resolve_edit_old_new_handles_incomplete_acp_snapshot() {
        let disk = "#![allow(dead_code)]\n// test comment\n\npub(crate) mod diff_window;\npub(crate) mod openusage;";
        let acp_new = "#![allow(dead_code)]\n// test comment\n\npub(crate) mod diff_window;";
        let pre_edit = "#![allow(dead_code)]\n\npub(crate) mod diff_window;\npub(crate) mod openusage;";

        let (old_full, new_full) =
            resolve_edit_old_new(None, acp_new.to_string(), Some(disk.to_string()));

        assert_eq!(new_full, disk);
        assert_eq!(old_full, pre_edit);
        let file_diff = file_diff_from_old_new(&old_full, &new_full, "mod.rs".to_string(), 2);
        let DiffType::Update { deltas, .. } = file_diff.diff_type else {
            panic!("expected update diff");
        };
        let inserted_lines: usize = deltas.iter().map(|d| d.insertion.lines().count()).sum();
        assert!(inserted_lines >= 1);
        assert!(
            deltas
                .iter()
                .any(|delta| delta.insertion.contains("// test comment"))
        );
        let deleted_lines: usize = deltas
            .iter()
            .map(|d| d.replacement_line_range.len())
            .sum();
        assert_eq!(deleted_lines, 0);
    }
}
