# Security Model

Lattice defaults to local, inert, user-controlled behavior.

## Default Deny

- No network calls.
- No auto-update HTTP requests.
- No terminal.
- No shell command execution.
- No plugin runtime.
- No runnable code blocks.
- No Mermaid JavaScript runtime.
- No remote Git push or pull.
- No arbitrary external path opening from note links without confirmation.

## Filesystem Access

Allowed access is limited to:

- The selected vault.
- The app config directory.
- Temporary files needed for atomic writes or exports.
- An explicitly chosen export path.

Vault-relative paths must use `VaultPath`, which rejects absolute paths and `..`. Symlinks are not followed outside the vault by default.

## Markdown Rendering

Markdown rendering should use a native render model. HTML scripts are never executed. Raw HTML, if supported later, must render inertly or behind an explicit setting.

## PDF Handling

PDFs are untrusted documents. Lattice should use a pure Rust viewing path where practical and must not support embedded JavaScript execution.

## Git History

Lattice uses an app-owned repository under `.lattice/history.git` by default. `.lattice/` and `.git/` are ignored by history. Existing user Git repositories are not modified unless a later expert mode explicitly enables that behavior.
