use lattice_core::VaultPath;
use ropey::Rope;

pub type TextBuffer = Rope;

#[derive(Debug, Clone)]
pub struct EditorBuffer {
    pub path: Option<VaultPath>,
    pub text: TextBuffer,
    pub dirty: bool,
    pub base_hash: blake3::Hash,
    pub base_modified_ms: u64,
}

impl EditorBuffer {
    pub fn new(path: Option<VaultPath>, contents: &str) -> Self {
        Self {
            path,
            text: Rope::from_str(contents),
            dirty: false,
            base_hash: blake3::hash(contents.as_bytes()),
            base_modified_ms: 0,
        }
    }

    pub fn contents(&self) -> String {
        self.text.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    Save,
    SaveAs,
    ReloadFromDisk,
    OverwriteDisk,
    OpenLinkUnderCursor,
    RenameCurrentFile,
    CloseTab,
}
