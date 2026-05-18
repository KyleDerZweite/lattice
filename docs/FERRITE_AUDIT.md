# Ferrite Audit

Ferrite is an MIT-licensed Rust `egui/eframe` editor and workspace project. Lattice uses it as an audited working reference, not as a cleanroom boundary.

Repository: https://github.com/OlaProeis/Ferrite

## Attribution

Any Ferrite-derived source copied or substantially adapted into Lattice must keep MIT attribution in the relevant file or module. New Lattice modules that only follow product concepts do not need file-level attribution but should be listed here when the audit is complete.

## Keep Or Adapt

- Rust native `egui/eframe` application structure.
- Rope-backed editor buffer.
- Virtual scrolling and line cache ideas.
- Undo/redo, selection, search highlights, bracket matching, line wrapping, and shaping paths after audit.
- Markdown preview, split view, image/PDF handling, Mermaid rendering, file tree, quick switcher, Git status indicators, autosave, session restore, wikilinks, backlinks, and export after audit.

## Remove Or Disable

- Integrated terminal.
- Shell pipeline or command execution.
- Runnable code blocks.
- Update checker network calls.
- AI-ready terminal indicators.
- LSP and broad code-editor positioning.
- Complex toolbar/ribbon surfaces that distract from vault editing.

## Initial Status

No Ferrite source has been copied into the current skeleton. The existing code only establishes crate boundaries, public models, and placeholder implementations.
