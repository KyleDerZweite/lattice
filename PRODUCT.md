# Lattice

Lattice is a fast, native code editor written in Rust. It is for people who want a VSCode-style editing experience — file tree, tabs, fuzzy file open, syntax highlighting — without Electron, WebView, integrated AI, extensions, accounts, telemetry, or plugin complexity.

The product is local-first, performance-focused, and security-conscious. Plain files are canonical; the app never executes file content, shell commands, plugins, or network sync.

## Product Goal

Build a minimal, native code editor that can be used daily, starts instantly, and stays light on memory and CPU.

The core promise:

- Open a folder, see its file tree (gitignore-aware), edit files.
- Syntax highlighting for common languages, line numbers, tabs, autosave.
- Safe saves: atomic writes, external-change detection, conflict resolution.
- Fuzzy quick open (Ctrl+P) over the whole workspace.
- Find/replace in one file and parallel search/replace across the workspace.
- No AI, no extensions, no network, no background services.

## Architecture

A Rust workspace of small crates on `egui/eframe`:

- `lattice-core` — path safety (`VaultPath` cannot escape the workspace root), settings.
- `lattice-workspace` — capability-scoped (`cap-std`) file access, lazy directory tree, parallel gitignore-aware search/walking, fuzzy quick-open index, file watcher, atomic writes with content-hash snapshots.
- `lattice-editor` — editor buffer model (dirty tracking, saved-snapshot hashes).
- `lattice-ui` — theme tokens and bundled fonts.
- `lattice-app` — the eframe app: sidebar tree + tabbed editor (egui_tiles), syntect highlighting, status bar, quick open, background worker thread for all I/O.

All filesystem work runs on a worker thread; the UI thread never blocks on I/O. Syntax highlighting is memoized and skipped above 1 MiB; files over 10 MB get a slow-edit warning.

## Done When

- Launch a native Linux app from a folder path or picker.
- Browse a lazy, gitignore-aware file tree; create, rename, delete files.
- Edit with syntax highlighting, line numbers, multiple tabs, autosave.
- Save safely with atomic writes and conflict detection against external edits.
- Quick-open any file with fuzzy search.
- Search and replace in the editor or across gitignore-aware workspace files.

## Non-Goals

- Integrated AI of any kind.
- Extensions/plugins, plugin execution.
- WebView or browser frontend runtime.
- Network access, telemetry, accounts, remote sync.
- Terminal or shell pipeline features.
- Runnable code blocks; realtime collaboration.
- Language servers and debuggers (may be revisited much later; not now).
