# Architecture

Lattice is a Rust workspace split by product boundaries:

- `lattice-app`: executable, CLI, logging, panic handling, `eframe` startup, lifecycle, and platform integration.
- `lattice-core`: shared IDs, errors, file metadata, settings types, and vault-relative path safety.
- `lattice-workspace`: vault opening, ignored path handling, file tree, CRUD, watcher integration, quick open, and external change detection.
- `lattice-editor`: rope-backed editor buffers and the native editing surface.
- `lattice-markdown`: Markdown parsing, wikilinks, headings, tags, frontmatter, backlinks, and preview data.
- `lattice-history`: app-managed Git-compatible snapshots under `.lattice/history.git`.
- `lattice-diff`: native diff model and `egui` diff viewer support.
- `lattice-graph`: note/link/tag graph data model and later native graph UI.
- `lattice-ui`: shared panels, command surfaces, styling, and reusable widgets.

The executable must not depend on npm, Tauri, Electron, WebView, or browser code. UI rendering is native `egui/eframe`.

## Data Ownership

Plain files in the selected vault are canonical. Lattice metadata stays in `.lattice/`. Index and cache data must be rebuildable. Local history is stored in `.lattice/history.git` by default, even when the vault already has a user `.git`.

## Safety Boundaries

All vault-facing APIs use `VaultPath` for vault-relative paths. `VaultPath` rejects absolute paths and `..` components. The workspace layer must not follow symlinks outside the vault unless a later explicit setting allows it.

File writes are atomic: write a sibling temp file, flush it, rename over the target, refresh metadata and content hash, then debounce self-generated watcher events.

## Ferrite Relationship

Ferrite is an audited MIT-licensed implementation reference. Lattice may selectively adapt code with attribution, but it removes terminal, shell execution, update-checking network calls, and broad code-editor features from the default product.
