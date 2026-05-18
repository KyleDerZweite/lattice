use lattice_core::VaultPath;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Unified,
    Split,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: VaultPath,
    pub old_label: String,
    pub new_label: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

pub fn diff_text(path: VaultPath, old: &str, new: &str) -> FileDiff {
    let mut old_line = 1;
    let mut new_line = 1;
    let lines = TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|change| match change.tag() {
            ChangeTag::Delete => {
                let line = DiffLine {
                    kind: DiffLineKind::Removed,
                    old_line: Some(old_line),
                    new_line: None,
                    text: change.to_string(),
                };
                old_line += 1;
                line
            }
            ChangeTag::Insert => {
                let line = DiffLine {
                    kind: DiffLineKind::Added,
                    old_line: None,
                    new_line: Some(new_line),
                    text: change.to_string(),
                };
                new_line += 1;
                line
            }
            ChangeTag::Equal => {
                let line = DiffLine {
                    kind: DiffLineKind::Context,
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    text: change.to_string(),
                };
                old_line += 1;
                new_line += 1;
                line
            }
        })
        .collect();

    FileDiff {
        path,
        old_label: "old".to_owned(),
        new_label: "new".to_owned(),
        hunks: vec![DiffHunk {
            old_start: 1,
            new_start: 1,
            lines,
        }],
    }
}
