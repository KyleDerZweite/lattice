# Architecture

Lattice is a Rust workspace split by product boundaries. Phase 0/1 only
implements the native app shell, settings, vault opening, and flat file listing;
later crate boundaries intentionally stay API-empty until their phase starts.

- `lattice-app`: executable, CLI, logging, panic handling, `eframe` startup, lifecycle, and platform integration.
- `lattice-core`: shared IDs, errors, file metadata, settings types, and vault-relative path safety.
- `lattice-workspace`: currently vault opening, ignored path handling, flat file listing, file reads, and atomic file creation. Planned: lazy tree loading, watcher integration, quick open, and external change detection.
- `lattice-editor`: Phase 3 boundary for rope-backed editor buffers and the native editing surface.
- `lattice-markdown`: currently wikilink extraction and an in-memory backlink sketch. Planned: headings, tags, frontmatter, backlinks, and preview data.
- `lattice-history`: Phase 6 boundary for app-managed Git-compatible snapshots under `.lattice/history.git`.
- `lattice-diff`: Phase 7 boundary for native diff models and `egui` diff viewer support.
- `lattice-graph`: Phase 9 boundary for note/link/tag graph data and native graph UI.
- `lattice-ui`: currently shared style setup. Planned: shared panels, command surfaces, and reusable widgets.

The executable must not depend on npm, Tauri, Electron, WebView, or browser code. UI rendering is native `egui/eframe`.

## Data Ownership

Plain files in the selected vault are canonical. Lattice metadata stays in `.lattice/`. Index and cache data must be rebuildable. Local history is stored in `.lattice/history.git` by default, even when the vault already has a user `.git`.

## Safety Boundaries

All vault-facing APIs use `VaultPath` for vault-relative paths. `VaultPath` rejects absolute paths and `..` components. The workspace layer must not follow symlinks outside the vault unless a later explicit setting allows it.

File writes are atomic: write a sibling temp file, flush it, rename over the target, refresh metadata and content hash, then debounce self-generated watcher events.

## Ferrite Relationship

Ferrite is an audited MIT-licensed implementation reference. Lattice may selectively adapt code with attribution, but it removes terminal, shell execution, update-checking network calls, and broad code-editor features from the default product.
