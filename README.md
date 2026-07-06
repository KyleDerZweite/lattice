# Lattice

A fast, minimal, native code editor. One ~17 MB binary, no Electron, no AI,
no extensions, no telemetry, no network.

## Features

- Lazy, gitignore-aware file tree
- Tabbed editing with syntax highlighting and line numbers
- Fuzzy quick open (`Ctrl+P`) over the whole workspace
- Find/replace in the active editor (`Ctrl+F` / `Ctrl+H`)
- Parallel, gitignore-aware search/replace across files (`Ctrl+Shift+F` / `Ctrl+Shift+H`)
- Autosave with atomic writes and conflict detection against external edits
- File watcher: external changes show up live
- Dark/light theme, bundled fonts — looks the same everywhere

All file I/O runs on a background thread; the UI never blocks. Workspace open
plus full file indexing takes milliseconds.

## Install

```sh
cargo build --release
install -Dm755 target/release/lattice ~/.local/bin/lattice
```

## Usage

```sh
lattice <folder>    # open a folder as workspace
lattice <file>      # open a file (parent folder becomes the workspace)
lattice --bench .   # headless performance benchmark
```

| Shortcut       | Action            |
| -------------- | ----------------- |
| `Ctrl+F`       | Find              |
| `Ctrl+H`       | Replace           |
| `Ctrl+Shift+F` | Find in files     |
| `Ctrl+Shift+H` | Replace in files  |
| `F3`           | Next match        |
| `Shift+F3`     | Previous match    |
| `Ctrl+P`       | Quick open        |
| `Ctrl+S`       | Save              |
| `Ctrl+N`       | New file          |
| `Ctrl+W`       | Close tab         |
| `Ctrl+Tab`     | Next tab          |
| `Ctrl+B`       | Toggle sidebar    |
| `Ctrl+O`       | Open folder       |

## Security

Local and inert by default: no network calls, no shell execution, no plugin
runtime. File access is capability-scoped to the opened folder and symlinks
are never followed out of it. See [docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md).

## License

[MIT](LICENSE). Bundled Adwaita fonts are licensed separately under the
[SIL OFL](assets/fonts/LICENSE).
