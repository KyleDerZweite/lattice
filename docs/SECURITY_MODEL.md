# Security Model

Lattice defaults to local, inert, user-controlled behavior.

## Default Deny

- No network calls.
- No auto-update HTTP requests.
- No terminal.
- No shell command execution.
- No plugin or extension runtime.
- No runnable code blocks.
- No integrated AI.

## Filesystem Access

Allowed access is limited to:

- The selected workspace folder (opened through `cap-std`, so all file
  operations are capability-scoped to that directory handle).
- The app config directory (settings only).
- Sibling temporary files needed for atomic writes.

Workspace-relative paths must use `VaultPath`, which rejects absolute paths and
`..`. Symlinks are never followed outside the workspace: every path component
is checked before open, save, rename, and delete.

## File Content

Opened files are treated as inert text. Lattice never executes, evaluates, or
renders file content as anything other than syntax-highlighted text.
