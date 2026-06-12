use anyhow::{anyhow, bail, Context, Result};
use camino::Utf8PathBuf;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, Metadata, OpenOptions};
use cap_tempfile::TempFile;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use lattice_core::OpenFileSnapshot;
use lattice_core::{AbsolutePath, VaultPath};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

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
    pub root: AbsolutePath,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub path: VaultPath,
    pub name: String,
    pub kind: TreeNodeKind,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNodeKind {
    File,
    DirectoryLoaded { children: Vec<TreeNode> },
    DirectoryUnloaded,
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
        let mut index = Self { files };
        index.files.sort();
        index.files.dedup();
        index
    }

    pub fn replace_all(&mut self, files: Vec<VaultPath>) {
        *self = Self::rebuild(files);
    }

    pub fn insert(&mut self, path: VaultPath) {
        match self.files.binary_search(&path) {
            Ok(_) => {}
            Err(index) => self.files.insert(index, path),
        }
    }

    pub fn remove(&mut self, path: &VaultPath) {
        if let Ok(index) = self.files.binary_search(path) {
            self.files.remove(index);
        }
    }

    pub fn rename(&mut self, from: &VaultPath, to: VaultPath) {
        self.remove(from);
        self.insert(to);
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<QuickOpenMatch> {
        if limit == 0 {
            return Vec::new();
        }
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
        let mut matches = Vec::with_capacity(limit);
        for path in &self.files {
            let Some(score) = matcher.fuzzy_match(path.as_str(), query) else {
                continue;
            };
            let candidate = QuickOpenMatch {
                path: path.clone(),
                score,
            };
            match matches
                .binary_search_by(|existing| compare_quick_open_matches(existing, &candidate))
            {
                Ok(index) | Err(index) => matches.insert(index, candidate),
            }
            if matches.len() > limit {
                matches.pop();
            }
        }
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

fn compare_quick_open_matches(a: &QuickOpenMatch, b: &QuickOpenMatch) -> std::cmp::Ordering {
    b.score
        .cmp(&a.score)
        .then_with(|| a.path.as_str().cmp(b.path.as_str()))
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
    watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<Event>>,
    vault_root: PathBuf,
    watched_paths: BTreeSet<PathBuf>,
}

impl WorkspaceWatcher {
    pub fn watch(vault_root: &Path) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(vault_root, RecursiveMode::NonRecursive)?;
        let mut watched_paths = BTreeSet::new();
        watched_paths.insert(vault_root.to_path_buf());
        Ok(Self {
            watcher,
            receiver,
            vault_root: vault_root.to_path_buf(),
            watched_paths,
        })
    }

    pub fn watch_path(&mut self, path: &Path) -> Result<()> {
        if self.watched_paths.contains(path) {
            return Ok(());
        }
        self.watcher.watch(path, RecursiveMode::NonRecursive)?;
        self.watched_paths.insert(path.to_path_buf());
        Ok(())
    }

    pub fn drain(&mut self) -> Vec<WorkspaceEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Ok(event) => {
                    let paths: Vec<_> = event
                        .paths
                        .into_iter()
                        .filter(|path| !is_ignored_path(&self.vault_root, path))
                        .collect();
                    if !paths.is_empty() {
                        events.push(WorkspaceEvent {
                            kind: map_event_kind(&event.kind),
                            paths,
                        });
                    }
                }
                Err(error) => log_notify_error(error),
            }
        }
        events
    }
}

pub struct Workspace {
    vault: Vault,
    root_dir: Dir,
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
        let root_dir = Dir::open_ambient_dir(&root, ambient_authority())
            .with_context(|| format!("opening vault directory {}", root.display()))?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Vault")
            .to_owned();
        Ok(Self {
            vault: Vault {
                root: AbsolutePath::new(root)?,
                name,
            },
            root_dir,
        })
    }

    pub fn try_clone_for_worker(&self) -> Result<Self> {
        Ok(Self {
            vault: self.vault.clone(),
            root_dir: self.root_dir.try_clone()?,
        })
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn watch(&self) -> Result<WorkspaceWatcher> {
        WorkspaceWatcher::watch(self.vault.root.as_path())
    }

    pub fn list_tree(&self, path: Option<&VaultPath>) -> Result<Vec<TreeNode>> {
        let relative = path.map(vault_path_to_std).unwrap_or_default();
        self.reject_symlink_path(&relative, true)?;
        let entries_result = if relative.as_os_str().is_empty() {
            self.root_dir.entries()
        } else {
            self.root_dir.read_dir(&relative)
        };
        let entries = match entries_result {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied && path.is_none() => {
                log::warn!(
                    "skipping unreadable directory {}",
                    self.display_path(&relative).display()
                );
                return Ok(Vec::new());
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "reading directory {}",
                        self.display_path(&relative).display()
                    )
                });
            }
        };

        let mut nodes = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    log::warn!(
                        "skipping unreadable directory entry in {}",
                        self.display_path(&relative).display()
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                log::warn!(
                    "skipping non-UTF-8 path in {}",
                    self.display_path(&relative).display()
                );
                continue;
            };
            let entry_relative = relative.join(name_str);
            if is_ignored_relative(&entry_relative) {
                continue;
            }
            let metadata = match self.root_dir.symlink_metadata(&entry_relative) {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
                    ) =>
                {
                    log::warn!(
                        "skipping inaccessible path {}",
                        self.display_path(&entry_relative).display()
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let relative = match utf8_relative_path(&entry_relative) {
                Ok(relative) => relative,
                Err(error) => {
                    log::warn!("{error}");
                    continue;
                }
            };
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
                warning: self.directory_warning(&entry_relative),
            });
        }
        nodes.sort_by(compare_tree_nodes);
        Ok(nodes)
    }

    pub fn list_files(&self) -> Result<Vec<TreeNode>> {
        let root = self.vault.root.as_path();
        let mut builder = WalkBuilder::new(root);
        builder
            .standard_filters(true)
            .require_git(false)
            .follow_links(false)
            .threads(2)
            .overrides(default_ignore_overrides(root)?);

        let (sender, receiver) = mpsc::channel();
        builder.build_parallel().run(|| {
            let sender = sender.clone();
            Box::new(move |result| {
                let _ = sender.send(result.map(|entry| entry.into_path()));
                ignore::WalkState::Continue
            })
        });
        drop(sender);

        let mut nodes = Vec::new();
        for result in receiver {
            let entry_path = match result {
                Ok(path) => path,
                Err(error) if is_permission_denied_ignore_error(&error) => {
                    log::warn!("skipping unreadable path");
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if entry_path == root || is_ignored_path(root, &entry_path) {
                continue;
            }
            let metadata = match fs::symlink_metadata(&entry_path) {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
                    ) =>
                {
                    log::warn!("skipping inaccessible path {}", entry_path.display());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let relative = match vault_relative_path(root, &entry_path) {
                Ok(relative) => relative,
                Err(error) => {
                    log::warn!("{error}");
                    continue;
                }
            };
            let path = VaultPath::new(&relative)?;
            nodes.push(TreeNode {
                name: relative
                    .file_name()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| relative.to_string()),
                path,
                kind: TreeNodeKind::File,
                warning: None,
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
        let (contents, _) = self.open_file(path)?;
        Ok(contents)
    }

    pub fn open_file(&self, path: &VaultPath) -> Result<(String, OpenFileSnapshot)> {
        let relative = vault_path_to_std(path);
        self.reject_symlink_path(&relative, true)?;
        let bytes = self
            .root_dir
            .read(&relative)
            .with_context(|| format!("reading {}", path.as_str()))?;
        let metadata = self
            .root_dir
            .symlink_metadata(&relative)
            .with_context(|| format!("snapshotting {}", path.as_str()))?;
        if !metadata.is_file() {
            bail!("not a file: {}", path.as_str());
        }
        let snapshot = snapshot_from_bytes(&metadata, &bytes);
        let contents = String::from_utf8(bytes)
            .with_context(|| format!("{} is not valid UTF-8", path.as_str()))?;
        Ok((contents, snapshot))
    }

    pub fn file_snapshot(&self, path: &VaultPath) -> Result<Option<OpenFileSnapshot>> {
        let relative = vault_path_to_std(path);
        if let Some(parent) = relative.parent() {
            self.reject_existing_symlink_components(parent)?;
        }
        file_snapshot_at(&self.root_dir, &relative)
    }

    pub fn create_file(&self, path: &VaultPath, contents: &str) -> Result<()> {
        let (parent, file_name) = self.open_parent_dir(path)?;
        let mut file = parent
            .open_with(&file_name, OpenOptions::new().write(true).create_new(true))
            .with_context(|| format!("creating {}", path.as_str()))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    pub fn save_file(&self, path: &VaultPath, contents: &str) -> Result<()> {
        self.reject_symlink(path)?;
        let (parent, file_name) = self.open_parent_dir(path)?;
        atomic_write(&parent, &file_name, contents)
    }

    pub fn create_directory(&self, path: &VaultPath) -> Result<()> {
        let relative = vault_path_to_std(path);
        self.reject_existing_symlink_components(&relative)?;
        self.root_dir
            .create_dir_all(&relative)
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
        let relative = vault_path_to_std(path);
        self.reject_symlink_path(&relative, true)?;
        let metadata = self
            .root_dir
            .symlink_metadata(&relative)
            .with_context(|| format!("checking {}", path.as_str()))?;
        if metadata.is_dir() {
            self.root_dir
                .remove_dir_all(&relative)
                .with_context(|| format!("deleting directory {}", path.as_str()))?;
        } else {
            self.root_dir
                .remove_file(&relative)
                .with_context(|| format!("deleting {}", path.as_str()))?;
        }
        Ok(())
    }

    fn rename_path(&self, from: &VaultPath, to: &VaultPath) -> Result<()> {
        let from_relative = vault_path_to_std(from);
        self.reject_symlink_path(&from_relative, true)?;
        self.reject_symlink(to)?;
        let (to_parent, to_name) = self.open_parent_dir(to)?;
        self.root_dir
            .rename(&from_relative, &to_parent, &to_name)
            .with_context(|| format!("renaming {} to {}", from.as_str(), to.as_str()))?;
        Ok(())
    }

    fn open_parent_dir(&self, path: &VaultPath) -> Result<(Dir, OsString)> {
        let relative = vault_path_to_std(path);
        let file_name = relative
            .file_name()
            .ok_or_else(|| anyhow!("path has no file name: {}", path.as_str()))?
            .to_os_string();
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        self.reject_existing_symlink_components(parent)?;
        if !parent.as_os_str().is_empty() {
            self.root_dir
                .create_dir_all(parent)
                .with_context(|| format!("creating parent directory for {}", path.as_str()))?;
            self.reject_existing_symlink_components(parent)?;
        }
        let parent_dir = if parent.as_os_str().is_empty() {
            self.root_dir.try_clone()?
        } else {
            self.root_dir
                .open_dir(parent)
                .with_context(|| format!("opening parent directory for {}", path.as_str()))?
        };
        Ok((parent_dir, file_name))
    }

    fn reject_symlink(&self, path: &VaultPath) -> Result<()> {
        self.reject_symlink_path(&vault_path_to_std(path), false)
    }

    fn reject_existing_symlink_components(&self, path: &Path) -> Result<()> {
        self.reject_symlink_path(path, false)
    }

    fn reject_symlink_path(&self, path: &Path, require_exists: bool) -> Result<()> {
        let mut current = PathBuf::new();
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        for component in path.components() {
            match component {
                Component::Normal(part) => current.push(part),
                Component::CurDir => continue,
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    bail!("invalid vault path component in {}", path.display());
                }
            }
            match self.root_dir.symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!(
                        "refusing to access symlinked vault path: {}",
                        path.display()
                    );
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound && !require_exists => {
                    return Ok(());
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("checking {}", path.display()))
                }
            }
        }
        Ok(())
    }

    fn directory_warning(&self, path: &Path) -> Option<String> {
        let Ok(metadata) = self.root_dir.symlink_metadata(path) else {
            return Some("Cannot read metadata".to_owned());
        };
        if metadata.file_type().is_symlink() {
            return Some("Symlink not followed".to_owned());
        }
        if !metadata.is_dir() {
            return None;
        }
        match self.root_dir.read_dir(path) {
            Ok(_) => None,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                Some("Permission denied".to_owned())
            }
            Err(error) => Some(error.to_string()),
        }
    }

    fn display_path(&self, relative: &Path) -> PathBuf {
        self.vault.root.as_path().join(relative)
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

fn atomic_write(parent: &Dir, file_name: &OsStr, contents: &str) -> Result<()> {
    let mut temp_file = TempFile::new(parent)?;
    temp_file.write_all(contents.as_bytes())?;
    temp_file.as_file().sync_all()?;
    temp_file.replace(file_name)?;
    Ok(())
}

fn file_snapshot_at(root: &Dir, path: &Path) -> Result<Option<OpenFileSnapshot>> {
    let metadata = match root.symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!("refusing to snapshot symlinked file: {}", path.display());
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut file = root.open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(OpenFileSnapshot {
        modified_ms: modified_ms(&metadata),
        size_bytes: metadata.len(),
        content_hash: hasher.finalize(),
    }))
}

fn snapshot_from_bytes(metadata: &Metadata, contents: &[u8]) -> OpenFileSnapshot {
    OpenFileSnapshot {
        modified_ms: modified_ms(metadata),
        size_bytes: metadata.len(),
        content_hash: blake3::hash(contents),
    }
}

fn modified_ms(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| {
            modified
                .into_std()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .ok()
        })
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn vault_path_to_std(path: &VaultPath) -> PathBuf {
    path.as_path().as_std_path().to_path_buf()
}

fn vault_relative_path(root: &Path, path: &Path) -> Result<Utf8PathBuf> {
    let relative = path.strip_prefix(root)?;
    Utf8PathBuf::from_path_buf(relative.to_path_buf())
        .map_err(|path| anyhow!("non-UTF-8 vault path: {}", path.display()))
}

fn utf8_relative_path(path: &Path) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|path| anyhow!("non-UTF-8 vault path: {}", path.display()))
}

fn is_permission_denied_ignore_error(error: &ignore::Error) -> bool {
    error
        .io_error()
        .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
}

fn default_ignore_overrides(root: &Path) -> Result<ignore::overrides::Override> {
    let mut overrides = OverrideBuilder::new(root);
    for ignored in DEFAULT_IGNORES {
        overrides.add(&format!("!{ignored}"))?;
        overrides.add(&format!("!{ignored}/**"))?;
    }
    Ok(overrides.build()?)
}

pub fn is_ignored_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    is_ignored_relative(relative)
}

fn is_ignored_relative(relative: &Path) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_vault() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lattice-workspace-test-{}-{nonce}-{counter}",
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
    fn open_file_returns_content_snapshot() {
        let root = temp_vault();
        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let path = VaultPath::try_from("note.md").unwrap();

        workspace.create_file(&path, "hello").unwrap();
        let (contents, snapshot) = workspace.open_file(&path).unwrap();

        assert_eq!(contents, "hello");
        assert_eq!(snapshot.size_bytes, 5);
        assert_eq!(snapshot.content_hash, blake3::hash(b"hello"));
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
    fn quick_open_mutates_index_and_limits_search_results() {
        let alpha = VaultPath::try_from("alpha.md").unwrap();
        let beta = VaultPath::try_from("beta.md").unwrap();
        let renamed = VaultPath::try_from("renamed-beta.md").unwrap();
        let mut index = QuickOpenIndex::rebuild(vec![alpha.clone(), beta.clone(), alpha.clone()]);

        assert_eq!(index.len(), 2);
        index.rename(&beta, renamed.clone());
        index.remove(&alpha);
        index.insert(VaultPath::try_from("gamma.md").unwrap());

        let matches = index.search("md", 1);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].path == renamed || matches[0].path.as_str() == "gamma.md");
    }

    #[test]
    fn list_files_honors_gitignore_hidden_default_ignores_and_symlinks() {
        let root = temp_vault();
        fs::write(root.join(".gitignore"), "ignored.md\nignored-dir/\n").unwrap();
        fs::write(root.join("note.md"), "visible").unwrap();
        fs::write(root.join("ignored.md"), "ignored").unwrap();
        fs::write(root.join(".hidden.md"), "hidden").unwrap();
        fs::create_dir_all(root.join("ignored-dir")).unwrap();
        fs::write(root.join("ignored-dir/note.md"), "ignored").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/output.md"), "ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("note.md"), root.join("link.md")).unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let files = workspace.list_files().unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();

        assert_eq!(paths, vec!["note.md"]);
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

    #[cfg(unix)]
    #[test]
    fn list_files_skips_permission_denied_directories() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_vault();
        let locked = root.join("data/postgres");
        fs::create_dir_all(&locked).unwrap();
        fs::write(root.join("note.md"), "visible").unwrap();
        fs::write(locked.join("internal.md"), "hidden").unwrap();

        let mut permissions = fs::metadata(&locked).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&locked, permissions).unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let files = workspace.list_files().unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();

        let mut permissions = fs::metadata(&locked).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&locked, permissions).unwrap();

        assert_eq!(paths, vec!["note.md"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_paths_are_rejected_for_file_operations() {
        let root = temp_vault();
        let outside = temp_vault();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.md"), root.join("file-link.md")).unwrap();
        std::os::unix::fs::symlink(root.join("missing.md"), root.join("dangling.md")).unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let linked_child = VaultPath::try_from("linked/secret.md").unwrap();
        let file_link = VaultPath::try_from("file-link.md").unwrap();
        let dangling = VaultPath::try_from("dangling.md").unwrap();
        let normal = VaultPath::try_from("normal.md").unwrap();
        let target = VaultPath::try_from("renamed.md").unwrap();

        assert!(workspace.open_file(&linked_child).is_err());
        assert!(workspace.save_file(&linked_child, "x").is_err());
        assert!(workspace.delete_file(&file_link).is_err());
        assert!(workspace.create_file(&dangling, "x").is_err());
        workspace.create_file(&normal, "ok").unwrap();
        assert!(workspace.rename_file(&normal, &file_link).is_err());
        workspace.rename_file(&normal, &target).unwrap();

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_are_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = temp_vault();
        fs::write(root.join("note.md"), "visible").unwrap();
        fs::write(
            root.join(OsString::from_vec(b"bad-\xFF.md".to_vec())),
            "bad",
        )
        .unwrap();

        let workspace = Workspace::open_vault(root.clone()).unwrap();
        let files = workspace.list_files().unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();

        assert_eq!(paths, vec!["note.md"]);
        fs::remove_dir_all(root).unwrap();
    }
}
