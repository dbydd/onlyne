use std::ops::Range;

/// Convert a byte span into a one-based line number.
pub(crate) fn line_from_span(source: &str, span: Option<Range<usize>>) -> usize {
    let idx = span.map(|range| range.start).unwrap_or(0).min(source.len());
    1 + source.as_bytes()[..idx]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

/// Find the line where a top-level table array entry starts.
pub(crate) fn array_entry_lines(source: &str, table_name: &str) -> Vec<usize> {
    let needle = format!("[[{table_name}]]");
    source
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed == needle || trimmed.starts_with(&(needle.clone() + " #")) {
                Some(idx + 1)
            } else {
                None
            }
        })
        .collect()
}

/// Find the line of a key inside a table-array entry.
pub(crate) fn key_line_in_array_entry(
    source: &str,
    table_name: &str,
    entry_index: usize,
    key: &str,
) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let starts = array_entry_lines(source, table_name);
    let Some(start_line) = starts.get(entry_index).copied() else {
        return 1;
    };
    let start_idx = start_line.saturating_sub(1);
    let end_idx = starts
        .get(entry_index + 1)
        .map(|line| line.saturating_sub(1))
        .unwrap_or(lines.len());
    key_line_between(&lines, start_idx, end_idx, key).unwrap_or(start_line)
}

/// Find the line of a key inside a top-level table.
pub(crate) fn key_line_in_table(source: &str, table_name: &str, key: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let header = format!("[{table_name}]");
    let Some(start_idx) = lines.iter().position(|line| line.trim() == header) else {
        return 1;
    };
    let end_idx = lines
        .iter()
        .enumerate()
        .skip(start_idx + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(lines.len());
    key_line_between(&lines, start_idx + 1, end_idx, key).unwrap_or(start_idx + 1)
}

fn key_line_between(lines: &[&str], start: usize, end: usize, key: &str) -> Option<usize> {
    let prefix = format!("{key} =");
    let dotted = format!("{key}=");
    lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .find_map(|(idx, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with(&prefix) || trimmed.starts_with(&dotted) {
                Some(idx + 1)
            } else {
                None
            }
        })
}
