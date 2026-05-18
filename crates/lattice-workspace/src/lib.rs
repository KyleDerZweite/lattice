use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use lattice_core::{AbsolutePath, FileKind, VaultId, VaultPath};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

pub const DEFAULT_IGNORES: &[&str] = &[
    ".git",
    ".lattice/history.git",
    ".lattice/index",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".cache",
    ".next",
    ".turbo",
];

#[derive(Debug, Clone)]
pub struct Vault {
    pub id: VaultId,
    pub root: AbsolutePath,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub path: VaultPath,
    pub name: String,
    pub kind: TreeNodeKind,
    pub expanded: bool,
    pub git_status: Option<GitStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNodeKind {
    File,
    DirectoryLoaded { children: Vec<TreeNode> },
    DirectoryUnloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatus {
    Clean,
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

pub struct Workspace {
    vault: Vault,
}

impl Workspace {
    pub fn open_vault(path: PathBuf) -> Result<Self> {
        let root = path
            .canonicalize()
            .with_context(|| format!("opening vault {}", path.display()))?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Vault")
            .to_owned();
        Ok(Self {
            vault: Vault {
                id: VaultId::default(),
                root: AbsolutePath::new(root),
                name,
            },
        })
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn list_files(&self) -> Result<Vec<TreeNode>> {
        let root = self.vault.root.as_path();
        let mut nodes = Vec::new();
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_ignored(root, entry))
        {
            let entry = entry?;
            if entry.path() == root || entry.file_type().is_dir() {
                continue;
            }
            let relative = entry.path().strip_prefix(root)?;
            let relative = Utf8PathBuf::from_path_buf(relative.to_path_buf())
                .map_err(|path| anyhow::anyhow!("non-UTF-8 vault path: {}", path.display()))?;
            let path = VaultPath::new(&relative)?;
            nodes.push(TreeNode {
                name: relative
                    .file_name()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| relative.to_string()),
                path,
                kind: TreeNodeKind::File,
                expanded: false,
                git_status: None,
            });
        }
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(nodes)
    }

    pub fn read_file(&self, path: &VaultPath) -> Result<String> {
        fs::read_to_string(path.join_to(self.vault.root.as_path()))
            .with_context(|| format!("reading {}", path.as_str()))
    }

    pub fn create_file(&self, path: &VaultPath, contents: &str) -> Result<()> {
        let absolute = path.join_to(self.vault.root.as_path());
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(absolute, contents)?;
        Ok(())
    }
}

fn is_ignored(root: &Path, entry: &DirEntry) -> bool {
    let Ok(relative) = entry.path().strip_prefix(root) else {
        return true;
    };
    let normalized = relative.to_string_lossy().replace('\\', "/");
    DEFAULT_IGNORES
        .iter()
        .any(|ignored| normalized == *ignored || normalized.starts_with(&format!("{ignored}/")))
}

pub fn kind_for_path(path: &VaultPath) -> FileKind {
    FileKind::from_path(path.as_path())
}
