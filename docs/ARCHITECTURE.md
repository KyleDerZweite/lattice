# Architecture

Lattice is a Rust workspace split by product boundaries. Everything ships in one
binary; there are no runtime plugins, services, or network dependencies. UI
rendering is native `egui/eframe` — no npm, Tauri, Electron, WebView, or
browser code.

- `lattice-app`: executable, CLI (`lattice [PATH]`, `--bench`), logging, panic
  handling, `eframe` startup, and the whole UI: sidebar file tree + tabbed code
  editor (split via `egui_tiles`), syntect syntax highlighting with a
  line-number gutter, editor find/replace, workspace search sidebar,
  quick-open overlay, menu bar, status bar.
- `lattice-core`: shared errors, settings types, and workspace-relative path
  safety (`VaultPath` rejects absolute, escaping, and non-UTF-8 paths).
- `lattice-workspace`: capability-scoped (`cap-std`) folder access, lazy
  directory tree listing, parallel gitignore-aware file walking and content
  search/replace, fuzzy quick-open index, `notify` file watcher, atomic writes,
  and blake3 content-hash snapshots for external-change/conflict detection.
- `lattice-editor`: editor buffer model (text, dirty flag, saved snapshot).
- `lattice-ui`: theme tokens (dark/light palettes mapped onto `egui::Style`)
  and bundled Adwaita fonts.

## Threading model

The UI thread never does workspace file I/O (the deliberate exceptions are
opening a folder and writing the small settings file — both one-shot, direct
user actions). `lattice-app` spawns one worker thread per opened workspace;
commands (load tree, open/save/create/rename/delete, build quick-open index,
check external changes) flow over an mpsc channel and responses are drained on
the UI thread, capped per frame. Responses carry a workspace generation so
stale results from a previously opened folder are dropped. Content searches
run as cancellable tasks over the parallel file walker, so a new query does
not wait for an older large-workspace query; replace-in-files likewise runs on
its own thread so a long replace does not stall saves or opens.

## Safety boundaries

All workspace-facing APIs use `VaultPath` for root-relative paths. The
workspace layer refuses to follow symlinks out of the opened folder.

File writes are atomic: write a sibling temp file, flush it, carry over the
target's permission bits, rename over the target, then refresh metadata and
content hash. Saves compare the on-disk snapshot against the buffer's base
snapshot and surface a conflict instead of clobbering external edits.

## Performance guards

- Syntax highlighting is memoized by egui and skipped entirely above 1 MiB.
- Files over 10 MB show a slow-edit warning.
- The directory tree loads lazily per expanded directory; the watcher watches
  only the root and expanded directories, non-recursively.
- Tree refreshes and external-change checks are debounced (250 ms).
- Workspace content search is debounced (180 ms), cancellable, bounded to
  10,000 displayed matches, and skips binary or larger-than-20-MiB files.
- Open editor buffers replace their disk copies in workspace search results;
  replace-in-files saves them through normal conflict detection.
- Release builds use fat LTO, one codegen unit, and stripped symbols.
