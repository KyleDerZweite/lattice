# Lattice

Lattice is a native Rust Markdown knowledge workspace for people who want plain-file notes, fast local editing, backlinks, recoverable history, and a graph-oriented model without Electron, WebView, accounts, remote sync, or plugin complexity.

The product is vault-first, Markdown-first, local-first, performance-focused, and security-conscious. Plain files are canonical. App metadata lives under `.lattice/`, derived indexes are rebuildable, and app-managed history uses `.lattice/history.git` by default so an existing user `.git` repository is not polluted.

## Product Goal

Build a simple, native Markdown workspace that can be used daily as a local editor and can later grow into graph, packaging, and optional sync capabilities.

The core promise:

- Your notes are plain Markdown files.
- Your local history is visible and recoverable.
- Your existing Git repository is untouched by default.
- Your graph and indexes can be rebuilt from the vault.
- The app does not execute note content, shell commands, plugins, or network sync by default.

## Current Direction

Lattice is being rebuilt as a Rust-native desktop app using `egui/eframe`. Ferrite is the audited working reference for editor, workspace, and Markdown ideas, with MIT attribution where code is adapted. Pierre Trees and Pierre Diffs are product design references only. Pretext and Code Storage are not runtime dependencies for the first rewrite.

## First Usable Release

The first release is done when a user can:

- Launch a native Linux app.
- Open a folder vault.
- Create, edit, rename, and delete Markdown notes.
- Save safely with atomic writes and conflict detection.
- Use `[[wikilinks]]` to open or create notes.
- See backlinks for the current note.
- Preview Markdown, Mermaid, images, and PDFs offline.
- Store local snapshots under `.lattice/history.git`.
- View file history and diffs.
- Restore one file from history.

Graph view, packaging polish, Windows support, and optional remote sync follow after the editor, workspace, history, and diff core are stable.

## Non-Goals For The First Release

- WebView or browser frontend runtime.
- npm runtime dependency.
- Network access by default.
- Automatic remote sync.
- Plugin execution.
- Terminal or shell pipeline features.
- Runnable code blocks.
- Realtime collaboration.
- Writing snapshots into a user's existing `.git` by default.
