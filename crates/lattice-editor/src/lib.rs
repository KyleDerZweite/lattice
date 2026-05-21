use lattice_core::{OpenFileSnapshot, VaultPath};

#[derive(Debug, Clone)]
pub struct EditorBuffer {
    pub path: Option<VaultPath>,
    pub text: String,
    pub dirty: bool,
    pub base_snapshot: Option<OpenFileSnapshot>,
}

impl EditorBuffer {
    pub fn from_disk(path: VaultPath, text: String, base_snapshot: OpenFileSnapshot) -> Self {
        Self {
            path: Some(path),
            text,
            dirty: false,
            base_snapshot: Some(base_snapshot),
        }
    }

    pub fn mark_saved(&mut self, snapshot: OpenFileSnapshot) {
        self.base_snapshot = Some(snapshot);
        self.dirty = false;
    }

    pub fn content_hash(&self) -> blake3::Hash {
        blake3::hash(self.text.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_buffer_tracks_hash_and_saved_state() {
        let path = VaultPath::try_from("note.md").unwrap();
        let snapshot = OpenFileSnapshot {
            modified_ms: 1,
            size_bytes: 5,
            content_hash: blake3::hash(b"hello"),
        };
        let mut buffer = EditorBuffer::from_disk(path, "hello".to_owned(), snapshot.clone());

        assert!(!buffer.dirty);
        assert_eq!(buffer.content_hash(), snapshot.content_hash);

        buffer.text.push_str(" world");
        buffer.dirty = true;
        let saved = OpenFileSnapshot {
            modified_ms: 2,
            size_bytes: 11,
            content_hash: blake3::hash(b"hello world"),
        };
        buffer.mark_saved(saved);

        assert!(!buffer.dirty);
        assert_eq!(buffer.base_snapshot.unwrap().size_bytes, 11);
    }
}
