# Lattice Rust Rewrite Plan: Ferrite-Informed Native Markdown Workspace

## Summary

Rebuild Lattice as a native Rust desktop application using Ferrite as the audited working reference, not the generated Tauri/Svelte code currently in this repo.

Chosen direction:

- Delete the current generated Tauri/Svelte implementation.
- Build a new Rust-native app with `egui/eframe`.
- Selectively port/adapt useful Ferrite modules under MIT attribution.
- Keep the product vault-first, Markdown-first, local-first, performance-focused, and security-conscious.
- Default local history storage: `vault/.lattice/history.git`.
- First usable release: editor + workspace + wikilinks/backlinks + local Git snapshots + diff/history UI.
- Graph view follows after the file/index/history core is stable.

This is not a true cleanroom rewrite. Ferrite is MIT licensed, so the correct model is a selective rewrite with clear attribution and code audit.

Primary references:

- Ferrite: https://github.com/OlaProeis/Ferrite
- Ferrite site/features: https://getferrite.dev/
- Ferrite SignPath page: https://signpath.org/projects/ferrite/
- Pierre Trees docs: https://trees.software/docs
- Pierre Diffs docs: https://diffs.com/docs
- Code Storage intro: https://code.storage/changelog/introducing-code-storage
- Pretext: https://github.com/chenglou/pretext

## Current Repo Handling

Delete the generated implementation before starting the rewrite:

- Remove `apps/desktop/`
- Remove root `package.json`
- Remove `pnpm-lock.yaml`
- Remove `pnpm-workspace.yaml`
- Remove generated Tauri/Svelte-specific docs if obsolete, especially `docs/EXTREME_MVP.md`

Keep or rewrite:

- Keep `PRODUCT.md`, but update it to reflect the Rust-native Ferrite-informed direction.
- Keep `.gitignore`, but replace Node/Tauri-specific ignores with Rust/native build ignores.
- Add `docs/ARCHITECTURE.md`
- Add `docs/ROADMAP.md`
- Add `docs/FERRITE_AUDIT.md`
- Add `docs/SECURITY_MODEL.md`
- Add `docs/HISTORY_MODEL.md`

## Product Definition

Lattice is a native Rust Markdown knowledge workspace.

The first real product should feel like:

- A fast native Markdown editor.
- A clean folder/vault workspace.
- A local knowledge graph system in progress.
- A versioned writing environment where history is automatic and recoverable.
- A simpler, more focused Ferrite/Obsidian-style tool without Electron, WebView, accounts, remote sync, or plugin complexity.

## Non-Negotiable Principles

- Plain files are canonical.
- Markdown remains readable outside Lattice.
- No WebView in the desktop app.
- No npm runtime dependency.
- No network access by default.
- No automatic remote sync.
- No executing code from notes in the default product.
- No shell pipeline, terminal, or runnable code block features in the first Lattice release.
- All file writes are atomic.
- External disk edits must never silently overwrite local edits.
- App-managed Git history must not pollute a user’s existing `.git` by default.
- App metadata must stay under `.lattice/`.
- Derived indexes must be rebuildable.

## Target Platforms

First-class for the rewrite:

- Linux desktop

Supported soon after:

- Windows desktop

Deferred:

- macOS polish
- Android
- self-hosted sync
- realtime collaboration

Linux packaging targets:

- plain release binary
- `.tar.gz`
- `.deb`
- `.rpm`
- AppImage if practical after the base release works

## New Repository Structure

```text
lattice/
  Cargo.toml
  Cargo.lock
  crates/
    lattice-app/
    lattice-core/
    lattice-editor/
    lattice-markdown/
    lattice-workspace/
    lattice-history/
    lattice-diff/
    lattice-graph/
    lattice-ui/
  assets/
    icons/
    themes/
  docs/
    ARCHITECTURE.md
    FERRITE_AUDIT.md
    HISTORY_MODEL.md
    ROADMAP.md
    SECURITY_MODEL.md
  tests/
    fixtures/
```

## Crate Responsibilities

### `lattice-app`

Owns the executable.

Responsibilities:

- `eframe` startup
- CLI parsing
- logging
- panic handling
- app lifecycle
- platform integration
- opening files/folders from arguments

Dependencies:

- `eframe`
- `egui`
- `clap`
- `log`
- `env_logger`
- `directories`

### `lattice-core`

Shared domain types.

Responsibilities:

- paths
- errors
- settings model
- app events
- file metadata
- common IDs
- safe vault-relative path handling

Important types:

```rust
pub struct VaultId(pub uuid::Uuid);

pub struct VaultPath {
    relative: camino::Utf8PathBuf,
}

pub struct AbsolutePath {
    absolute: std::path::PathBuf,
}

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

pub struct FileMeta {
    pub path: VaultPath,
    pub kind: FileKind,
    pub modified_ms: u64,
    pub size_bytes: u64,
    pub content_hash: Option<blake3::Hash>,
}
```

Path rules:

- Reject absolute paths where vault-relative paths are expected.
- Reject `..`.
- Normalize separators.
- Treat symlinks cautiously.
- Never follow symlinks outside the vault unless explicitly allowed later.

### `lattice-editor`

Native rope-backed editor.

Reference Ferrite modules:

- `src/editor/ferrite/buffer.rs`
- `src/editor/ferrite/editor.rs`
- `src/editor/ferrite/history.rs`
- `src/editor/ferrite/line_cache.rs`
- `src/editor/ferrite/view.rs`
- `src/editor/ferrite/shaping.rs`

Keep/adapt:

- `ropey` buffer
- virtual scrolling
- line cache
- undo/redo
- selection
- multi-cursor if stable
- IME/CJK shaping path
- line wrapping
- search highlights
- bracket matching

Cut for first release unless already cleanly isolated:

- Vim mode
- LSP diagnostics
- terminal-specific hooks
- executable code block integration

Public API:

```rust
pub struct EditorBuffer {
    pub path: Option<VaultPath>,
    pub text: TextBuffer,
    pub dirty: bool,
    pub base_hash: blake3::Hash,
    pub base_modified_ms: u64,
}

pub enum EditorAction {
    Save,
    SaveAs,
    ReloadFromDisk,
    OverwriteDisk,
    OpenLinkUnderCursor,
    RenameCurrentFile,
    CloseTab,
}
```

### `lattice-workspace`

Vault and filesystem layer.

Reference Ferrite modules:

- `src/workspaces/file_tree.rs`
- `src/workspaces/watcher.rs`
- `src/workspaces/mod.rs`
- `src/ui/quick_switcher.rs`

Responsibilities:

- open vault
- create vault
- list files
- lazy file tree loading
- file CRUD
- watcher events
- ignored path handling
- quick-open index
- recent files
- external change detection

Default ignored paths:

```text
.git
.lattice/history.git
.lattice/index
node_modules
target
dist
build
out
.cache
.next
.turbo
```

File tree identity must be path-first, inspired by Pierre Trees:

```rust
pub struct TreeNode {
    pub path: VaultPath,
    pub name: String,
    pub kind: TreeNodeKind,
    pub expanded: bool,
    pub git_status: Option<GitStatus>,
}

pub enum TreeNodeKind {
    File,
    DirectoryLoaded { children: Vec<TreeNode> },
    DirectoryUnloaded,
}
```

Do not port `@pierre/trees` directly. Recreate the useful ideas natively:

- canonical path identity
- stable selection/focus by path
- lazy expansion
- keyboard navigation
- rename-in-place
- Git status decorations
- dense rows
- low visual noise

### `lattice-markdown`

Markdown parsing, preview data, wikilinks, backlinks.

Reference Ferrite modules:

- `src/markdown/parser.rs`
- `src/markdown/editor.rs`
- `src/ui/backlinks_panel.rs`
- `src/state.rs` backlink index section

Responsibilities:

- parse Markdown
- extract headings
- extract wikilinks
- extract Markdown links
- extract tags
- extract frontmatter
- provide preview render model
- build backlink data
- provide graph edges later

Supported wikilinks in first release:

```md
[[Note]]
[[folder/Note]]
[[Note|Alias]]
[[Note#Heading]]
```

Not required in first release:

```md
![[Embed]]
[[Note^block-id]]
```

Wikilink resolution order:

1. Exact vault-relative Markdown file path.
2. Exact path plus `.md`.
3. Current folder relative path.
4. Unique basename match.
5. If multiple matches, show picker.
6. If none exists, create new note at target path.

### `lattice-history`

App-managed Git snapshot layer.

Default storage:

```text
vault/
  notes.md
  folder/note.md
  .lattice/
    config.toml
    ignore
    history.git/
    index/
```

Use Git with:

```text
--git-dir=.lattice/history.git
--work-tree=.
```

Preferred implementation:

- Use `gix` if it supports the required local snapshot operations cleanly.
- Fall back to `git2` if `gix` becomes slower to implement.
- Do not shell out to system `git` in normal app behavior.

First release history features:

- initialize history repo
- maintain default ignore file
- detect dirty tracked/untracked note files
- stage changed allowed files
- auto-commit after idle period
- force checkpoint on `Ctrl+S` if there are pending changes
- checkpoint before risky operations
- list commits
- show file history
- show diff for a file between commits
- restore one file from a commit

Commit policy:

- Autosave writes files.
- History commit is coalesced.
- Do not commit every keystroke.
- Default idle snapshot delay: 60 seconds.
- Minimum time between autosnapshot commits: 30 seconds.
- `Ctrl+S` saves immediately and schedules/flushes a snapshot if content changed.
- Risky operations create pre-operation checkpoints:
  - delete file
  - rename file
  - move folder
  - bulk import
  - external conflict resolution overwrite

Commit messages:

```text
Lattice autosnapshot: 2026-05-18 14:32
Lattice checkpoint: before deleting notes/foo.md
Lattice checkpoint: renamed old.md to new.md
Manual checkpoint: <user text>
```

Default `.lattice/ignore`:

```gitignore
.lattice/
.git/
node_modules/
target/
dist/
build/
out/
.cache/
.next/
.turbo/
*.tmp
*.swp
.DS_Store
Thumbs.db
```

Important rule:

- If a vault already has a user `.git`, Lattice still uses `.lattice/history.git` by default.
- Later expert mode may allow using the existing `.git`.

### `lattice-diff`

Native diff engine and diff UI.

Do not embed `@pierre/diffs`. Recreate its useful product ideas natively:

- side-by-side diff
- unified diff
- hunk headers
- line numbers
- syntax-colored Markdown/code blocks where reasonable
- selected hunk restore
- selected file restore
- clear additions/removals
- low-noise colors
- virtualized large diffs

Suggested crates:

- `similar` for text diffs
- `syntect` for syntax highlighting if already used
- custom `egui` renderer

Core types:

```rust
pub enum DiffMode {
    Unified,
    Split,
}

pub struct FileDiff {
    pub path: VaultPath,
    pub old_label: String,
    pub new_label: String,
    pub hunks: Vec<DiffHunk>,
}

pub struct DiffHunk {
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}
```

### `lattice-graph`

Graph index and graph UI.

First release may include the data model but not the graph view.

Responsibilities:

- build graph nodes from notes
- build edges from wikilinks and Markdown links
- expose backlinks
- expose local graph around current note
- expose global graph later

Data model:

```rust
pub enum GraphNodeKind {
    Note,
    Heading,
    Tag,
}

pub struct GraphNode {
    pub id: GraphNodeId,
    pub label: String,
    pub path: Option<VaultPath>,
    pub kind: GraphNodeKind,
}

pub enum GraphEdgeKind {
    Wikilink,
    MarkdownLink,
    Tag,
    HeadingLink,
}

pub struct GraphEdge {
    pub from: GraphNodeId,
    pub to: GraphNodeId,
    pub kind: GraphEdgeKind,
}
```

Graph rendering should be native `egui` first.

Do not use Cytoscape/Sigma/React Flow in the Rust-native rewrite.

### `lattice-ui`

Shared UI components.

Responsibilities:

- top bar
- file tree
- command palette
- quick open
- tabs
- status bar
- settings
- conflict banner
- history panel
- diff viewer
- backlinks panel
- preview pane
- PDF viewer panel
- Mermaid preview panel

UI style:

- quiet
- dense
- minimal
- no marketing page
- no card-heavy dashboard
- no decorative gradients
- no nested cards
- no oversized hero UI
- 4-8 px radii
- keyboard-first
- native desktop feel

Default layout:

```text
┌──────────────────────────────────────────────┐
│ top bar: vault / current file / status / cmd │
├──────────────┬───────────────────────────────┤
│ file tree    │ editor / preview / diff area  │
│ backlinks    │                               │
└──────────────┴───────────────────────────────┘
```

Initial panels:

- left sidebar: file tree
- optional lower-left tab: backlinks/history
- main area: editor
- optional split preview
- command palette overlay

## Ferrite Feature Decisions

Keep or adapt:

- Rust + `egui/eframe`
- rope-backed custom editor
- virtual scrolling
- Markdown preview
- split view
- native Mermaid
- PDF viewer
- image viewer
- file tree
- quick switcher
- Git status indicators
- autosave
- session restore
- wikilinks/backlinks, after audit
- export to PDF, after audit

Remove or disable by default:

- integrated terminal
- shell pipeline / command execution
- runnable code blocks
- update checker network call
- AI-ready terminal indicators
- LSP
- broad code-editor positioning
- overly complex toolbar/ribbon UI
- anything that creates network access

Keep as optional later:

- Vim mode
- CSV/TSV viewer
- JSON/YAML/TOML tree viewer
- snippets
- minimap
- custom keyboard shortcuts

## Pretext Decision

Do not port Pretext for the first Rust rewrite.

Reason:

- Pretext is a JavaScript/TypeScript text layout library aimed at browser/Canvas/SVG layout.
- Ferrite already has native text shaping work through `harfrust`, `unicode-segmentation`, and `egui`.
- Porting Pretext to Rust would be a separate text-layout project and would delay the product.

Use Pretext only as conceptual inspiration for later layout measurement tests:

- stable line measurement
- virtualization correctness
- no layout shift
- mixed-language text cases

## Code Storage Decision

Do not integrate Code Storage in the local-first product.

Use its idea, not its service:

- Git is a good snapshot primitive.
- Content-addressed history is useful for recovery, diffs, rollback, and future AI workflows.
- Lattice should own local Git-compatible snapshots first.

Future optional interface:

```rust
pub trait SnapshotRemote {
    fn push(&self, repo: &HistoryRepo) -> Result<()>;
    fn pull(&self, repo: &HistoryRepo) -> Result<()>;
    fn list_refs(&self) -> Result<Vec<RemoteRef>>;
}
```

Possible future implementations:

- self-hosted Git
- GitHub
- Code Storage
- local bare remote

No remote implementation in the first release.

## App Data and Vault Metadata

Inside vault:

```text
.lattice/
  config.toml
  ignore
  history.git/
  index/
  cache/
```

`config.toml`:

```toml
version = 1
history_enabled = true
autosnapshot_idle_seconds = 60
theme = "system"
editor_font_size = 14
editor_font_family = "monospace"
```

Rules:

- `.lattice/` is ignored by Lattice history.
- `.lattice/` is hidden from the normal note tree by default.
- If deleted, Lattice can rebuild indexes but not local history.
- Warn before deleting `.lattice/history.git` from inside the app.

## File Safety Model

Each opened editor buffer stores:

```rust
pub struct OpenFileSnapshot {
    pub modified_ms: u64,
    pub size_bytes: u64,
    pub content_hash: blake3::Hash,
}
```

Save flow:

1. User edits buffer.
2. Autosave debounce fires or `Ctrl+S` is pressed.
3. Check current disk metadata/hash.
4. If disk matches base snapshot, write atomically.
5. If disk changed and buffer is dirty, show conflict UI.
6. If disk changed and buffer is clean, reload.
7. After successful write, update base snapshot.
8. Schedule history snapshot.

Atomic write flow:

1. Write to temp file beside target.
2. Flush file.
3. Rename over target.
4. Refresh metadata/hash.
5. Debounce self-generated watcher events.

Conflict UI actions:

- Reload from disk
- Overwrite disk
- Save as copy
- Show diff

Deleted file behavior:

- If clean: close tab.
- If dirty: keep buffer open as unsaved and show “File deleted on disk.”

## Commands and Shortcuts

Default shortcuts:

```text
Ctrl+O        Open vault/folder
Ctrl+N        New note
Ctrl+S        Save and checkpoint soon
Ctrl+Shift+S Manual checkpoint
Ctrl+P        Quick open
Ctrl+K        Command palette
Ctrl+F        Find in file
Ctrl+Shift+F Search workspace
Ctrl+W        Close tab
F2            Rename selected file
Delete        Delete selected file after confirmation
Ctrl+B        Toggle backlinks panel
Ctrl+H        Open history panel
Ctrl+D        Open diff for current file
Ctrl+Click    Open wikilink
```

Command palette commands:

- Open Vault
- New Note
- Rename Note
- Delete Note
- Save
- Manual Checkpoint
- Show File History
- Show Diff Since Last Snapshot
- Toggle Preview
- Toggle Backlinks
- Toggle Theme
- Open Settings

## UI Screens

### First Launch

Show a minimal start screen:

- Open Folder
- Create Folder
- Recent folders if any

No account prompt.

### Main Editor

Visible:

- vault name
- current path
- saved/unsaved/snapshot status
- left tree
- editor

Optional:

- preview split
- backlinks panel
- history panel

### History Panel

For current file:

- list commits touching the file
- timestamp
- message
- changed line count if cheap
- buttons:
  - View Diff
  - Restore File
  - Copy Version

For vault:

- recent snapshots
- manual checkpoints
- risky-operation checkpoints

### Diff View

Modes:

- unified
- split

Actions:

- restore whole file
- copy old version
- copy selected lines
- open current file

First release does not need interactive hunk accept/reject.

### Backlinks Panel

Show:

- source note
- surrounding line/snippet
- click to open source

### Graph View

Not in first usable release.

Planned next:

- local graph around current note
- global graph
- filter by folder/tag
- click node to open note

## Security Model

Default-deny capabilities:

- No network calls.
- No auto-update HTTP request.
- No terminal.
- No shell command execution.
- No plugin execution.
- No Mermaid JavaScript runtime.
- No remote Git push/pull.
- No opening arbitrary external paths from note links without confirmation.

Allowed filesystem access:

- selected vault
- app config directory
- temp files for atomic writes/export
- explicitly chosen export path

Markdown rendering:

- Do not execute HTML scripts.
- Prefer native Markdown render model over raw HTML.
- If raw HTML is supported, render inertly or behind a setting.

PDF:

- Use pure Rust viewer path where possible.
- Treat malformed PDFs as untrusted input.
- No embedded JavaScript support.

Git:

- Use app-owned repo by default.
- Never commit secrets outside allowed vault files.
- Respect `.lattice/ignore`.
- Ignore `.git/` and `.lattice/`.

## Performance Targets

First release targets:

- Linux cold start under 1 second on ordinary hardware.
- Open 1,000-note vault under 500 ms for initial usable tree.
- Quick open over 10,000 files under 50 ms after index built.
- Smooth typing for Markdown files under 1 MB.
- Large file warning above 10 MB.
- No full-vault parsing on every keystroke.
- File watcher events batched/debounced.
- Backlink index updated incrementally on save.

Large file behavior:

- open in raw mode
- disable live preview if needed
- disable live wikilink parsing if needed
- keep save/history safe

## Implementation Phases

### Phase 0: Repo Reset and Planning Docs

Tasks:

- Delete generated Tauri/Svelte implementation.
- Create Rust workspace.
- Add new docs.
- Add MIT attribution note for Ferrite-derived code.
- Add `docs/FERRITE_AUDIT.md`.

Acceptance:

- No Node/Tauri app remains.
- `cargo metadata` works.
- Repo direction is clear from docs.

### Phase 1: Native App Skeleton

Tasks:

- Create `lattice-app`.
- Start `eframe`.
- Add theme.
- Add top bar/sidebar/main editor placeholders.
- Add settings persistence.
- Add CLI open folder/file.

Acceptance:

- `cargo run -p lattice-app` opens a native window.
- App starts on Linux.
- No network dependency.
- No WebView.

### Phase 2: Workspace and File Tree

Tasks:

- Implement vault open/create.
- Implement path-safe file listing.
- Implement lazy tree.
- Implement ignored directories.
- Implement create/rename/delete/move.
- Implement watcher.
- Implement quick open.

Acceptance:

- Open real folder.
- Tree displays files.
- Markdown files can be created, renamed, deleted.
- External file creation/removal updates tree.
- Quick open works.

### Phase 3: Editor Port

Tasks:

- Port/adapt Ferrite rope buffer.
- Port/adapt editor widget.
- Add tabs.
- Add dirty state.
- Add save/reload.
- Add autosave.
- Add external change handling.
- Add large-file warning.

Acceptance:

- Edit Markdown daily without crashes.
- Undo/redo works.
- Autosave works.
- External conflict shows choices.
- No visible typing jank for normal notes.

### Phase 4: Markdown Preview, PDF, Mermaid

Tasks:

- Port/adapt Markdown parser/render model.
- Add raw/rendered/split modes.
- Add native Mermaid render path.
- Add PDF viewer tab.
- Add image viewer tab.
- Keep export optional until stable.

Acceptance:

- Markdown preview renders common syntax.
- Mermaid diagrams render offline.
- PDF files open in viewer tab.
- Broken Mermaid/PDF input does not crash app.

### Phase 5: Wikilinks and Backlinks

Tasks:

- Extract wikilinks from Markdown.
- Highlight/click wikilinks in rendered mode.
- Add open/create target behavior.
- Add backlink index.
- Add backlinks panel.
- Add alias and heading parsing.

Acceptance:

- `[[Note]]` opens or creates `Note.md`.
- `[[Note|Alias]]` displays alias and opens target.
- `[[Note#Heading]]` resolves target note.
- Backlinks update after save.

### Phase 6: Local Git History

Tasks:

- Initialize `.lattice/history.git`.
- Add `.lattice/ignore`.
- Stage allowed files.
- Commit autosnapshots.
- Commit manual checkpoints.
- Commit before risky operations.
- Show snapshot status in status bar.
- Show Git status in file tree.

Acceptance:

- First vault open initializes history after confirmation or default setting.
- Saves create coalesced snapshots.
- Delete/rename creates pre-operation checkpoint.
- User `.git` is untouched.
- `.lattice/` is not included in history.

### Phase 7: Diff and Restore UI

Tasks:

- Implement `lattice-diff`.
- Add file history view.
- Add unified diff.
- Add split diff.
- Add restore file from snapshot.
- Add conflict diff view.

Acceptance:

- User can view previous versions.
- User can compare current file to last snapshot.
- User can restore one file.
- Conflict can be reviewed before overwrite/reload.

### Phase 8: Polish and Packaging

Tasks:

- Linux `.deb`
- Linux `.rpm`
- Linux `.tar.gz`
- AppImage spike
- icon/desktop file
- file associations
- settings UI
- keyboard shortcut help
- smoke test script

Acceptance:

- Fresh Linux install works.
- App can be used as daily Markdown editor.
- Build artifacts are reproducible enough for local release.

### Phase 9: Graph View

Tasks:

- Build graph index from notes/wikilinks/tags.
- Add local graph view.
- Add global graph view.
- Add filters.
- Add node click navigation.
- Add graph refresh after save.

Acceptance:

- Current note graph is useful.
- Global graph opens for medium vaults.
- Graph does not slow down editing.

## Testing Plan

### Rust Unit Tests

Cover:

- vault-relative path normalization
- escaping path rejection
- symlink handling
- ignored directory matching
- file type detection
- atomic writes
- external-change detection
- watcher event filtering
- wikilink parsing
- wikilink resolution
- backlink extraction
- Git history initialization
- ignore rules
- autosnapshot coalescing
- diff hunk generation
- restore single file

### Integration Tests

Use temp vaults.

Scenarios:

- open empty vault
- create note
- edit and save note
- autosnapshot after idle
- rename note creates checkpoint
- delete note creates checkpoint
- restore deleted note
- external clean change reloads
- external dirty change shows conflict
- diff current vs previous version
- backlink updates after save
- quick open finds note

### UI Smoke Tests

Manual first, automated later.

Linux smoke:

- launch app
- open notes folder
- create/edit/rename/delete note
- use quick open
- use wikilink
- show backlinks
- save and confirm history snapshot
- view diff
- restore previous version
- open PDF
- render Mermaid
- close and reopen session

### Performance Tests

Add benchmark fixtures:

- 1,000 Markdown files
- 10,000 Markdown files
- 1 MB note
- 10 MB note
- note with many wikilinks
- large diff

Track:

- startup time
- vault open time
- quick open latency
- save latency
- snapshot latency
- memory after vault open
- diff render time

## Dependency Defaults

Initial dependency candidates:

```toml
eframe = "0.31"
egui = "0.31"
ropey = "1.6"
notify = "6"
walkdir = "2"
camino = "1"
blake3 = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "2"
anyhow = "1"
directories = "5"
clap = { version = "4", features = ["derive"] }
fuzzy-matcher = "0.3"
similar = "2"
comrak = "0.22"
syntect = "5"
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp", "bmp"] }
hayro = "0.5"
```

Git dependency decision:

- Start with a tiny spike comparing `gix` and `git2`.
- Choose the first one that cleanly supports:
  - app-owned bare repo
  - work tree outside git dir
  - status
  - add/update index
  - commit
  - list commits
  - read blob at commit
  - diff commit/file versions

Default if spike is inconclusive:

- Use `git2` for first release because Ferrite already uses it and local status operations are proven there.

## Public Interfaces

### Workspace Service

```rust
pub trait WorkspaceService {
    fn open_vault(&mut self, path: AbsolutePath) -> Result<Vault>;
    fn list_tree(&self, path: Option<&VaultPath>) -> Result<Vec<TreeNode>>;
    fn open_file(&self, path: &VaultPath) -> Result<OpenFile>;
    fn save_file(&mut self, input: SaveFileInput) -> Result<SaveFileResult>;
    fn create_file(&mut self, path: &VaultPath, contents: &str) -> Result<OpenFile>;
    fn rename_file(&mut self, from: &VaultPath, to: &VaultPath) -> Result<()>;
    fn delete_file(&mut self, path: &VaultPath) -> Result<()>;
}
```

### History Service

```rust
pub trait HistoryService {
    fn init(vault_root: &Path) -> Result<Self>
    where
        Self: Sized;

    fn status(&self) -> Result<Vec<FileHistoryStatus>>;
    fn schedule_autosnapshot(&mut self, reason: SnapshotReason) -> Result<()>;
    fn checkpoint(&mut self, message: &str) -> Result<Option<CommitId>>;
    fn file_history(&self, path: &VaultPath) -> Result<Vec<HistoryEntry>>;
    fn diff_file(&self, path: &VaultPath, old: CommitId, new: DiffTarget) -> Result<FileDiff>;
    fn restore_file(&mut self, path: &VaultPath, commit: CommitId) -> Result<()>;
}
```

### Markdown Index

```rust
pub trait MarkdownIndex {
    fn rebuild(&mut self, files: &[VaultPath]) -> Result<()>;
    fn update_file(&mut self, path: &VaultPath, contents: &str) -> Result<()>;
    fn remove_file(&mut self, path: &VaultPath) -> Result<()>;
    fn backlinks(&self, path: &VaultPath) -> Vec<Backlink>;
    fn outgoing_links(&self, path: &VaultPath) -> Vec<LinkTarget>;
    fn graph_snapshot(&self) -> GraphSnapshot;
}
```

## Explicit Assumptions

- Current generated Tauri/Svelte code is disposable.
- Ferrite-derived code is allowed under MIT with attribution.
- Lattice should be Rust-native, not WebView-based.
- Linux is the first target.
- Windows follows after Linux is stable.
- Android is not part of this rewrite milestone.
- `.lattice/history.git` is the default snapshot store.
- Existing user `.git` repos are not modified by default.
- Pierre projects are design references, not runtime dependencies.
- Pretext is not ported now.
- Code Storage is future optional remote infrastructure, not MVP.
- Terminal/shell execution features are excluded for security.
- Graph view is important but comes after editor/history/diff stability.

## Done Criteria For First Usable Release

The first rewrite release is done when:

- Lattice launches as a native Linux app.
- A user can open a folder vault.
- A user can create, edit, rename, delete Markdown notes.
- Autosave works.
- External edits are detected safely.
- `[[wikilinks]]` open/create notes.
- Backlinks work.
- Markdown preview works.
- Mermaid works offline.
- PDF files open in-app.
- Local Git snapshots are created under `.lattice/history.git`.
- User `.git` is untouched.
- File history can be viewed.
- Diffs can be viewed.
- A single file can be restored from history.
- The UI is cleaner and more vault-focused than Ferrite.
- Linux package artifacts can be built.
- No network, shell execution, terminal, or plugin runtime is active by default.
