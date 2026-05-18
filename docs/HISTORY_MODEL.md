# History Model

Lattice stores local snapshots in an app-managed Git-compatible repository:

```text
vault/
  notes.md
  folder/note.md
  .lattice/
    config.toml
    ignore
    history.git/
    index/
```

Git operations use the equivalent of:

```text
--git-dir=.lattice/history.git
--work-tree=.
```

The first implementation should use `git2` unless a short `gix` spike proves simpler for the required local operations.

## Commit Policy

- Autosave writes files.
- History commits are coalesced.
- Do not commit every keystroke.
- Default idle snapshot delay is 60 seconds.
- Minimum time between autosnapshot commits is 30 seconds.
- `Ctrl+S` saves immediately and schedules or flushes a snapshot if content changed.
- Delete, rename, move folder, bulk import, and overwrite conflict actions create pre-operation checkpoints.

## Commit Messages

```text
Lattice autosnapshot: 2026-05-18 14:32
Lattice checkpoint: before deleting notes/foo.md
Lattice checkpoint: renamed old.md to new.md
Manual checkpoint: <user text>
```

## Default Ignore

```gitignore
.lattice/
.git/
node_modules/
target/
dist/
build/
out/
.cache/
.next/
.turbo/
*.tmp
*.swp
.DS_Store
Thumbs.db
```

## Restore

The first restore UI restores one file from a selected snapshot. Whole-vault restore and remote push/pull are deferred.
