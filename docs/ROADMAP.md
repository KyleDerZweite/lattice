# Roadmap

## Phase 0: Repo Reset and Planning Docs

- Remove the generated Tauri/Svelte implementation.
- Create the Rust workspace and crate boundaries.
- Add architecture, security, history, roadmap, and Ferrite audit docs.
- Ensure `cargo metadata` works.

## Phase 1: Native App Skeleton

- Start `eframe`.
- Add a quiet native layout with top bar, sidebar, and editor area.
- Add CLI folder/file opening.
- Add settings persistence.

## Phase 2: Workspace and File Tree

- Open and create vaults.
- List files with ignored paths.
- Add lazy tree loading, file CRUD, watcher events, and quick open.

## Phase 3: Editor

- Adapt Ferrite-inspired rope buffer/editor pieces.
- Add tabs, dirty state, save/reload, autosave, and external conflict UI.

## Phase 4: Markdown Preview, PDF, Mermaid

- Add Markdown preview data and native rendering.
- Add offline Mermaid rendering, image tabs, and PDF viewer tabs.

## Phase 5: Wikilinks and Backlinks

- Parse and resolve `[[wikilinks]]`.
- Add open/create target behavior and backlink indexing.

## Phase 6: Local Git History

- Initialize `.lattice/history.git`.
- Stage allowed files, coalesce autosnapshots, and create manual/risky-operation checkpoints.

## Phase 7: Diff and Restore

- Add unified and split diffs.
- Add file history and restore-one-file actions.

## Phase 8: Packaging

- Build Linux binary, `.tar.gz`, `.deb`, `.rpm`, and desktop integration.

## Phase 9: Graph View

- Build local and global graph views from Markdown links, wikilinks, tags, and headings.
