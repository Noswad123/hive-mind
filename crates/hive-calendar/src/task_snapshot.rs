//! Provider-neutral committed source facade. No projection or publication authority.
pub use crate::{SourceTask as CommittedTask, TaskMetadata};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub struct CommittedTaskSnapshot {
    pub commit: String,
    pub parser_sha256: String,
    pub blob_ids: BTreeMap<String, String>,
    pub items: Vec<CommittedTask>,
    /// Literal indented lines following each checkbox, before parser normalization.
    pub literal_metadata: BTreeMap<String, String>,
    pub stable_marker_counts: BTreeMap<String, usize>,
    /// Canonical standalone markers attached to their validated parser task IDs.
    pub stable_markers: BTreeMap<String, String>,
}

// MW excludes frontmatter and fenced examples from task/marker ownership. Preserve
// line slots so this evidence stays tied to the parser's committed line numbers.
fn source_lines(text: &str) -> Result<Vec<&str>> {
    let mut lines: Vec<_> = text.split('\n').collect();
    let prefix = if lines
        .first()
        .is_some_and(|line| line.trim_end_matches('\r') == "---")
    {
        lines
            .iter()
            .skip(1)
            .position(|line| line.trim_end_matches('\r') == "---")
            .context("unterminated frontmatter")?
            + 2
    } else {
        0
    };
    let mut fence = None;
    for (index, line) in lines.iter_mut().enumerate() {
        if index < prefix {
            *line = "";
            continue;
        }
        let trimmed = line.trim_start();
        let ch = trimmed.as_bytes().first().copied().unwrap_or_default();
        let count = trimmed.bytes().take_while(|b| *b == ch).count();
        let boundary = matches!(ch, b'`' | b'~') && count >= 3;
        if let Some((marker, size)) = fence {
            if boundary && ch == marker && count >= size && trimmed[count..].trim().is_empty() {
                fence = None;
            }
            *line = "";
        } else if boundary {
            fence = Some((ch, count));
            *line = "";
        }
    }
    Ok(lines)
}

/// Uses the same bounded isolated Git/blob/parser reader as Calendar, not the worktree.
pub fn read_committed_tasks(
    repo: &Path,
    commit: &str,
    mw_bin: &Path,
) -> Result<CommittedTaskSnapshot> {
    let snapshot = crate::source::read_snapshot(repo, commit, mw_bin)?;
    let mut literal_metadata = BTreeMap::new();
    let mut metadata_bytes = 0usize;
    let checkbox = regex::Regex::new(r"^\s*(?:[-*+]|[0-9]+[.)])\s*\[[ xX]\](?:\s|$)")?;
    let heading = regex::Regex::new(r"^(#+|\*+) \s*(\S.*)$")?;
    let indent = |line: &str| line.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
    let mut stable_marker_counts = BTreeMap::new();
    let mut stable_markers = BTreeMap::new();
    for (path, blob) in &snapshot.files {
        let text = std::str::from_utf8(&blob.content)?;
        let lines = source_lines(text)?;
        // Inventory spans historical/inactive Markdown too, but only Todo scopes
        // containing parsed tasks authorize active ownership/grammar validation.
        let mut todo = None;
        let scopes: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                if let Some(captures) = heading.captures(line.trim())
                    && captures[1].len() <= 2
                {
                    todo = (captures[1].len() == 2
                        && captures[2].trim().eq_ignore_ascii_case("Todo"))
                    .then_some(index);
                }
                todo
            })
            .collect();
        let mut tasks = BTreeMap::new();
        for task in snapshot
            .items
            .iter()
            .filter(|task| &task.source_path == path)
        {
            ensure!(
                tasks.insert(task.line, task).is_none(),
                "ambiguous task line ownership"
            );
            let mut metadata = Vec::new();
            let task_indent = indent(&task.literal_text);
            for line in lines.iter().skip(task.line) {
                if line.trim().is_empty() {
                    continue;
                }
                if indent(line) <= task_indent {
                    break;
                }
                ensure!(!checkbox.is_match(line), "unsupported nested checkbox task");
                if heading.is_match(line.trim()) {
                    break;
                }
                if !line.contains("mw-task:") {
                    metadata.push(*line);
                }
            }
            metadata_bytes += metadata.iter().map(|line| line.len() + 1).sum::<usize>();
            ensure!(
                metadata_bytes <= 64 * 1024 * 1024,
                "literal metadata size limit exceeded"
            );
            literal_metadata.insert(task.id.clone(), metadata.join("\n"));
        }
        let active_scopes: BTreeSet<_> = tasks
            .values()
            .filter_map(|task| scopes[task.line - 1])
            .collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("mw-task:") && !line.contains("<!-- mw-task") {
                continue;
            }
            let id = line
                .trim()
                .strip_prefix("<!-- mw-task:v1:")
                .and_then(|s| s.strip_suffix(" -->"))
                .filter(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                });
            if let Some(id) = id {
                *stable_marker_counts.entry(id.to_owned()).or_insert(0) += 1;
            }
            let owner = tasks
                .range(..=index)
                .next_back()
                .map(|(_, task)| *task)
                .filter(|owner| {
                    let task_indent = indent(&owner.literal_text);
                    indent(line) > task_indent
                        && lines[owner.line..=index].iter().all(|line| {
                            line.trim().is_empty()
                                || (indent(line) > task_indent
                                    && !checkbox.is_match(line)
                                    && !heading.is_match(line.trim()))
                        })
                });
            if owner.is_none() && !scopes[index].is_some_and(|scope| active_scopes.contains(&scope))
            {
                continue;
            }
            let id = id.context("malformed reserved task marker")?;
            let owner = owner.context("orphan task marker")?;
            ensure!(
                stable_markers
                    .insert(owner.id.clone(), id.to_owned())
                    .is_none(),
                "duplicate task markers in one block"
            );
        }
    }
    Ok(CommittedTaskSnapshot {
        commit: snapshot.commit,
        parser_sha256: snapshot.parser_sha256,
        blob_ids: snapshot
            .files
            .into_iter()
            .map(|(path, blob)| (path, blob.object_id))
            .collect(),
        items: snapshot.items,
        literal_metadata,
        stable_marker_counts,
        stable_markers,
    })
}
