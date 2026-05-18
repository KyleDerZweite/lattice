use anyhow::Result;
use lattice_core::VaultPath;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_HISTORY_DIR: &str = ".lattice/history.git";
pub const DEFAULT_IGNORE: &str = ".lattice/\n.git/\nnode_modules/\ntarget/\ndist/\nbuild/\nout/\n.cache/\n.next/\n.turbo/\n*.tmp\n*.swp\n.DS_Store\nThumbs.db\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: CommitId,
    pub message: String,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub struct HistoryRepo {
    vault_root: PathBuf,
}

impl HistoryRepo {
    pub fn init(vault_root: impl AsRef<Path>) -> Result<Self> {
        let vault_root = vault_root.as_ref().to_path_buf();
        fs::create_dir_all(vault_root.join(DEFAULT_HISTORY_DIR))?;
        let lattice_dir = vault_root.join(".lattice");
        fs::create_dir_all(&lattice_dir)?;
        fs::write(lattice_dir.join("ignore"), DEFAULT_IGNORE)?;
        Ok(Self { vault_root })
    }

    pub fn history_dir(&self) -> PathBuf {
        self.vault_root.join(DEFAULT_HISTORY_DIR)
    }

    pub fn file_history(&self, _path: &VaultPath) -> Result<Vec<HistoryEntry>> {
        Ok(Vec::new())
    }
}
