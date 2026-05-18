use anyhow::{anyhow, bail, Context, Result};
use camino::Utf8PathBuf;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use lattice_core::{AbsolutePath, FileKind, VaultId, VaultPath};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickOpenMatch {
    pub path: VaultPath,
    pub score: i64,
}

#[derive(Debug, Clone, Default)]
pub struct QuickOpenIndex {
    files: Vec<VaultPath>,
}

impl QuickOpenIndex {
    pub fn rebuild(files: Vec<VaultPath>) -> Self {
        Self { files }
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<QuickOpenMatch> {
        if query.trim().is_empty() {
            return self
                .files
                .iter()
                .take(limit)
                .cloned()
                .map(|path| QuickOpenMatch { path, score: 0 })
                .collect();
        }

        let matcher = SkimMatcherV2::default();
        let mut matches: Vec<_> = self
            .files
            .iter()
            .filter_map(|path| {
                matcher
                    .fuzzy_match(path.as_str(), query)
                    .map(|score| QuickOpenMatch {
                        path: path.clone(),
                        score,
                    })
            })
            .collect();
        matches.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.path.as_str().cmp(b.path.as_str()))
        });
        matches.truncate(limit);
        matches
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceEvent {
    pub kind: WorkspaceEventKind,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEventKind {
    Create,
    Modify,
    Remove,
    Rename,
    Other,
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<Event>>,
}

impl WorkspaceWatcher {
    pub fn watch(vault_root: &Path) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(vault_root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            receiver,
        })
    }

    pub fn drain(&mut self) -> Vec<WorkspaceEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Ok(event) => events.push(WorkspaceEvent {
                    kind: map_event_kind(&event.kind),
                    paths: event.paths,
                }),
                Err(error) => log_notify_error(error),
            }
        }
        events
    }
}

pub struct Workspace {
    vault: Vault,
}

impl Workspace {
    pub fn create_vault(path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&path).with_context(|| format!("creating vault {}", path.display()))?;
        Self::open_vault(path)
    }

    pub fn open_vault(path: PathBuf) -> Result<Self> {
        let root = path
            .canonicalize()
            .with_context(|| format!("opening vault {}", path.display()))?;
        if !root.is_dir() {
            bail!("vault path is not a directory: {}", root.display());
        }
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Vault")
            .to_owned();
        Ok(Self {
            vault: Vault {
                id: VaultId::default(),
                root: AbsolutePath::new(root)?,
                name,
            },
        })
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn watch(&self) -> Result<WorkspaceWatcher> {
        WorkspaceWatcher::watch(self.vault.root.as_path())
    }

    pub fn list_tree(&self, path: Option<&VaultPath>) -> Result<Vec<TreeNode>> {
        let absolute = match path {
            Some(path) => path.join_to(self.vault.root.as_path()),
            None => self.vault.root.as_path().to_path_buf(),
        };
        ensure_inside_vault(self.vault.root.as_path(), &absolute)?;
        if is_symlink(&absolute)? {
            bail!(
                "refusing to load symlinked tree path: {}",
                absolute.display()
            );
        }

        let mut nodes = Vec::new();
        for entry in fs::read_dir(&absolute)
            .with_context(|| format!("reading directory {}", absolute.display()))?
        {
            let entry = entry?;
            let entry_path = entry.path();
            if is_ignored_path(self.vault.root.as_path(), &entry_path) {
                continue;
            }
            let metadata = fs::symlink_metadata(&entry_path)?;
            let relative = vault_relative_path(self.vault.root.as_path(), &entry_path)?;
            let name = relative
                .file_name()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| relative.to_string());
            let kind = if metadata.is_dir() {
                TreeNodeKind::DirectoryUnloaded
            } else {
                TreeNodeKind::File
            };
            nodes.push(TreeNode {
                path: VaultPath::new(&relative)?,
                name,
                kind,
                expanded: false,
                git_status: None,
            });
        }
        nodes.sort_by(compare_tree_nodes);
        Ok(nodes)
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
            let relative = vault_relative_path(root, entry.path())?;
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

    pub fn quick_open_index(&self) -> Result<QuickOpenIndex> {
        let files = self
            .list_files()?
            .into_iter()
            .map(|node| node.path)
            .collect();
        Ok(QuickOpenIndex::rebuild(files))
    }

    pub fn read_file(&self, path: &VaultPath) -> Result<String> {
        let absolute = self.checked_absolute(path)?;
        if is_symlink(&absolute)? {
            bail!("refusing to read symlinked file: {}", path.as_str());
        }
        fs::read_to_string(absolute).with_context(|| format!("reading {}", path.as_str()))
    }

    pub fn create_file(&self, path: &VaultPath, contents: &str) -> Result<()> {
        let absolute = self.checked_absolute(path)?;
        if absolute.exists() {
            bail!("file already exists: {}", path.as_str());
        }
        atomic_write(&absolute, contents)
    }

    pub fn save_file(&self, path: &VaultPath, contents: &str) -> Result<()> {
        let absolute = self.checked_absolute(path)?;
        atomic_write(&absolute, contents)
    }

    pub fn create_directory(&self, path: &VaultPath) -> Result<()> {
        let absolute = self.checked_absolute(path)?;
        fs::create_dir_all(&absolute)
            .with_context(|| format!("creating directory {}", path.as_str()))?;
        Ok(())
    }

    pub fn rename_file(&self, from: &VaultPath, to: &VaultPath) -> Result<()> {
        self.rename_path(from, to)
    }

    pub fn move_path(&self, from: &VaultPath, to: &VaultPath) -> Result<()> {
        self.rename_path(from, to)
    }

    pub fn delete_file(&self, path: &VaultPath) -> Result<()> {
        let absolute = self.checked_absolute(path)?;
        let metadata = fs::symlink_metadata(&absolute)
            .with_context(|| format!("checking {}", path.as_str()))?;
        if metadata.is_dir() {
            fs::remove_dir_all(&absolute)
                .with_context(|| format!("deleting directory {}", path.as_str()))?;
        } else {
            fs::remove_file(&absolute).with_context(|| format!("deleting {}", path.as_str()))?;
        }
        Ok(())
    }

    fn rename_path(&self, from: &VaultPath, to: &VaultPath) -> Result<()> {
        let from_absolute = self.checked_absolute(from)?;
        let to_absolute = self.checked_absolute(to)?;
        if let Some(parent) = to_absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from_absolute, &to_absolute)
            .with_context(|| format!("renaming {} to {}", from.as_str(), to.as_str()))?;
        Ok(())
    }

    fn checked_absolute(&self, path: &VaultPath) -> Result<PathBuf> {
        let absolute = path.join_to(self.vault.root.as_path());
        ensure_inside_vault(self.vault.root.as_path(), &absolute)?;
        Ok(absolute)
    }
}

fn compare_tree_nodes(a: &TreeNode, b: &TreeNode) -> std::cmp::Ordering {
    let a_dir = matches!(
        a.kind,
        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
    );
    let b_dir = matches!(
        b.kind,
        TreeNodeKind::DirectoryLoaded { .. } | TreeNodeKind::DirectoryUnloaded
    );
    b_dir
        .cmp(&a_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.path.cmp(&b.path))
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = sibling_temp_path(path);
    {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn sibling_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lattice");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{nonce}.tmp"))
}

fn ensure_inside_vault(root: &Path, path: &Path) -> Result<()> {
    if !path.starts_with(root) {
        bail!("path escapes vault: {}", path.display());
    }
    Ok(())
}

fn is_symlink(path: &Path) -> Result<bool> {
    Ok(fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            }
        })?)
}

fn vault_relative_path(root: &Path, path: &Path) -> Result<Utf8PathBuf> {
    let relative = path.strip_prefix(root)?;
    Utf8PathBuf::from_path_buf(relative.to_path_buf())
        .map_err(|path| anyhow!("non-UTF-8 vault path: {}", path.display()))
}

fn is_ignored(root: &Path, entry: &DirEntry) -> bool {
    is_ignored_path(root, entry.path())
}

pub fn is_ignored_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let normalized = relative.to_string_lossy().replace('\\', "/");
    DEFAULT_IGNORES
        .iter()
        .any(|ignored| normalized == *ignored || normalized.starts_with(&format!("{ignored}/")))
}

fn map_event_kind(kind: &EventKind) -> WorkspaceEventKind {
    match kind {
        EventKind::Create(_) => WorkspaceEventKind::Create,
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => WorkspaceEventKind::Rename,
        EventKind::Modify(_) => WorkspaceEventKind::Modify,
        EventKind::Remove(_) => WorkspaceEventKind::Remove,
        _ => WorkspaceEventKind::Other,
    }
}

fn log_notify_error(error: notify::Error) {
    log::warn!("workspace watcher error: {error}");
}

pub fn kind_for_path(path: &VaultPath) -> FileKind {
    FileKind::from_path(path.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_vault() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lattice-workspace-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn list_files_skips_default_ignored_paths() {
        let root = temp_vault();
        fs::write(root.join("note.md"), "visible").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "ignored").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/output.md"), "ignored").unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let files = workspace.list_files().unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();

        assert_eq!(paths, vec!["note.md"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn list_tree_returns_lazy_directories_before_files() {
        let root = temp_vault();
        fs::write(root.join("b.md"), "visible").unwrap();
        fs::create_dir_all(root.join("a-folder")).unwrap();
        fs::write(root.join("a-folder/note.md"), "child").unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let tree = workspace.list_tree(None).unwrap();

        assert_eq!(tree[0].path.as_str(), "a-folder");
        assert!(matches!(tree[0].kind, TreeNodeKind::DirectoryUnloaded));
        assert_eq!(tree[1].path.as_str(), "b.md");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_file_writes_contents() {
        let root = temp_vault();
        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let path = VaultPath::try_from("folder/note.md").unwrap();

        workspace.create_file(&path, "hello").unwrap();

        assert_eq!(
            fs::read_to_string(root.join("folder/note.md")).unwrap(),
            "hello"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rename_and_delete_file() {
        let root = temp_vault();
        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let from = VaultPath::try_from("old.md").unwrap();
        let to = VaultPath::try_from("folder/new.md").unwrap();

        workspace.create_file(&from, "hello").unwrap();
        workspace.rename_file(&from, &to).unwrap();
        assert!(!root.join("old.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("folder/new.md")).unwrap(),
            "hello"
        );

        workspace.delete_file(&to).unwrap();
        assert!(!root.join("folder/new.md").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quick_open_finds_nested_note() {
        let root = temp_vault();
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("folder/project-plan.md"), "visible").unwrap();
        fs::write(root.join("other.md"), "visible").unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let index = workspace.quick_open_index().unwrap();
        let matches = index.search("proj", 5);

        assert_eq!(matches[0].path.as_str(), "folder/project-plan.md");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_vault_creates_missing_folder() {
        let root = temp_vault();
        let nested = root.join("new-vault");

        let workspace = Workspace::create_vault(nested.clone()).unwrap();

        assert_eq!(
            workspace.vault().root.as_path(),
            nested.canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
