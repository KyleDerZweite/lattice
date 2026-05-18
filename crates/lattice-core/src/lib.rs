use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LatticeError {
    #[error("vault-relative path must not be empty")]
    EmptyPath,
    #[error("absolute paths are not valid vault-relative paths: {0}")]
    AbsoluteVaultPath(String),
    #[error("path escapes the vault: {0}")]
    EscapingVaultPath(String),
    #[error("path contains an unsupported prefix: {0}")]
    UnsupportedPathPrefix(String),
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
}

pub type Result<T> = std::result::Result<T, LatticeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VaultId(pub uuid::Uuid);

impl Default for VaultId {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VaultPath {
    relative: Utf8PathBuf,
}

impl VaultPath {
    pub fn new(path: impl AsRef<Utf8Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.as_str().is_empty() {
            return Err(LatticeError::EmptyPath);
        }
        if path.is_absolute() {
            return Err(LatticeError::AbsoluteVaultPath(path.to_string()));
        }

        let mut normalized = Utf8PathBuf::new();
        for component in path.components() {
            match component {
                Utf8Component::Normal(part) => normalized.push(part),
                Utf8Component::CurDir => {}
                Utf8Component::ParentDir => {
                    return Err(LatticeError::EscapingVaultPath(path.to_string()));
                }
                Utf8Component::RootDir | Utf8Component::Prefix(_) => {
                    return Err(LatticeError::UnsupportedPathPrefix(path.to_string()));
                }
            }
        }

        if normalized.as_str().is_empty() {
            return Err(LatticeError::EmptyPath);
        }

        Ok(Self {
            relative: normalized,
        })
    }

    pub fn as_path(&self) -> &Utf8Path {
        &self.relative
    }

    pub fn as_str(&self) -> &str {
        self.relative.as_str()
    }

    pub fn join_to(&self, vault_root: &Path) -> PathBuf {
        vault_root.join(self.relative.as_std_path())
    }
}

impl TryFrom<&str> for VaultPath {
    type Error = LatticeError;

    fn try_from(value: &str) -> Result<Self> {
        Self::new(Utf8Path::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AbsolutePath {
    absolute: PathBuf,
}

impl AbsolutePath {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            absolute: path.into(),
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.absolute
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileKind {
    Markdown,
    Image,
    Pdf,
    Json,
    Yaml,
    Toml,
    Csv,
    Other,
}

impl FileKind {
    pub fn from_path(path: &Utf8Path) -> Self {
        match path
            .extension()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "md" | "markdown" => Self::Markdown,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Self::Image,
            "pdf" => Self::Pdf,
            "json" => Self::Json,
            "yaml" | "yml" => Self::Yaml,
            "toml" => Self::Toml,
            "csv" => Self::Csv,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub path: VaultPath,
    pub kind: FileKind,
    pub modified_ms: u64,
    pub size_bytes: u64,
    pub content_hash: Option<blake3::Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFileSnapshot {
    pub modified_ms: u64,
    pub size_bytes: u64,
    pub content_hash: blake3::Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    pub version: u32,
    pub history_enabled: bool,
    pub autosnapshot_idle_seconds: u64,
    pub theme: ThemeMode,
    pub editor_font_size: u32,
    pub editor_font_family: String,
    pub recent_vaults: Vec<PathBuf>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: 1,
            history_enabled: true,
            autosnapshot_idle_seconds: 60,
            theme: ThemeMode::System,
            editor_font_size: 14,
            editor_font_family: "monospace".to_owned(),
            recent_vaults: Vec::new(),
        }
    }
}

impl AppSettings {
    pub fn remember_vault(&mut self, path: PathBuf) {
        self.recent_vaults.retain(|recent| recent != &path);
        self.recent_vaults.insert(0, path);
        self.recent_vaults.truncate(10);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_relative_paths() {
        assert_eq!(
            VaultPath::try_from("folder/Note.md").unwrap().as_str(),
            "folder/Note.md"
        );
        assert_eq!(
            VaultPath::try_from("./Note.md").unwrap().as_str(),
            "Note.md"
        );
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(VaultPath::try_from("../secret.md").is_err());
        assert!(VaultPath::try_from("notes/../../secret.md").is_err());
    }
}
