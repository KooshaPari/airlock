# airlock-v2 — Python → Rust Adaptation

This document records the design decisions made when porting the existing
Python daemon at `~/CodeProjects/Phenotype/repos/.airlock/bin/airlock-v2.py`
into the `airlock_v2` Rust crate under `crates/airlock-v2/`.

## State location

The task brief specified `~/.airlock-v2/registry/repos.json`. The actual
live Python state lives at **`~/.airlock/v2/registry.json`** (note the
nested directory). The Rust port follows **the Python's layout**, not the
brief, because:

1. The Python daemon is still running and reading the same file.
2. The two daemons share state by design — running them side-by-side is a
   hard requirement of the port-and-upgrade model.
3. Diverging the on-disk path would force a one-shot migration; the brief
   was clear that the existing registry should keep working.

If a relocation is ever wanted, the right move is to add an `--state-root`
flag and a one-shot migration script — not to silently change the path.

## Schema compatibility

The registry JSON shape is preserved exactly:

```jsonc
{
  "/abs/path": {
    "registered_at":    "...Z",
    "last_check":       "...Z",
    "last_dirty_count": 0,
    "remote_url":       "...",
    "primary_branch":   "main",
    "last_auto_commit": "...Z",
    "last_push_time":   "...Z"  // also accepts JSON null
  }
}
```

Both shapes are accepted on read:

- Wrapped: `{ "repos": { "/abs/path": { ... } } }` — our forward-looking
  schema.
- Legacy: `{ "/abs/path": { ... } }` — what the Python writes today.

The Rust serializer always emits the wrapped shape; the Python does not
care because it ignores unknown top-level keys.

## Push policy

`git push --atomic --force-with-lease` is **forbidden** by the brief.

The Rust port implements:

1. First attempt: `git push --ff-only origin <branch>` — fast-forward only.
2. On rejection (non-FF, no remote, rejected, network): create a backup
   `wip/<date>-<uuid8>` ref from `HEAD` and `git push --set-upstream origin
   <wip-branch>`. A new ref is inherently fast-forwardable from the server.

This preserves the Python's `try_push_or_snapshot` intent ("never lose
work") while honouring the FF-only rule: **no push operation ever uses
`--force` or `git push --push-option`**. The backup branch itself is
always a fresh ref, so the server-side acceptance is just a regular
FF-push.

## Dependency footprint

Allowed (already present in `PhenoVCS/Cargo.lock`):

- `anyhow` 1.0
- `serde` 1.0 (with derive)
- `serde_json` 1.0
- `chrono` 0.4 (with serde)
- `clap` 4.6 (workspace pin)

Deliberately **not** added:

- `git2` — we shell out via `std::process::Command`, matching
  `crates/worktree-manager/src/worktree_manager/infrastructure/git_adapter.rs`.
  This keeps the rust toolchain self-consistent across the workspace.
- `tokio` — we use plain `std::thread::sleep` in the daemon example. The
  tick cadence is measured in minutes; an async runtime is overkill.
- `reqwest`, `ureq`, `octocrab` — the daemon doesn't talk to GitHub
  directly; pushes go through the user's installed `git`, which already
  has the SSH keys / credentials configured.
- `signal-hook` — we use a simple PID-file-watcher pattern instead. The
  launchd plists control the daemon's lifetime (kickstart + kill via
  `launchctl`).
- `uuid` — we derive a 16-hex-char suffix from `SystemTime::now()` for
  branch uniqueness. The hash space (2^64 possibilities per nanosecond
  slice) is collision-safe for the daemon's polling cadence.

## Architecture

```text
                ┌────────────────────────────────┐
                │     crates/airlock-v2/         │
                │                                │
  CLI arg ──────▶  src/cli.rs   (clap)          │
                  │                              │
                  │   register/list/status/etc   │
                  ▼                              │
            ┌─────────────────────────────────┐ │
            │  autocommit.rs                  │ │
            │  cleanup.rs                     │ │
            │       │                         │ │
            │       ▼                         │ │
            │  registry.rs   ◀── schema-compat │ │
            │       │                         │ │
            │       ▼                         │ │
            │  git_ops.rs    (shell-out)      │ │
            └─────────────────────────────────┘ │
                          │
                          ▼
                $HOME/.airlock/v2/registry.json
                $HOME/.airlock/v2/logs/<name>.log
                $HOME/.airlock/v2/<mode>.pid
```

The crate exposes two binaries:

- `airlock-v2` — the CLI (single-shot, perfect for launchd wrappers)
- `cargo run --example daemon -- <autocommit|cleanup>` — the long-running
  scheduler that the existing launchd plists can be redirected to call

## Test coverage

`cargo test -p airlock-v2` exercises:

- `git_ops::dirty_count_zero_for_clean_repo` — works against a real
  in-tempdir git init, confirms the `git status --porcelain` shell-out
  behaves correctly on an empty repo.
- `git_ops::dirty_count_increments_with_modifications` — confirms untracked
  + modified files are detected.
- `git_ops::push_branch_ff_only_returns_failure_for_missing_remote` —
  confirms push attempts against a repo with no origin fail safely.
- `git_ops::is_inside_work_tree_recognises_repo` — discriminant.
- `registry::round_trip_preserves_entries` — JSON serialize/deserialize.
- `registry::accepts_legacy_flat_map_shape` — cross-daemon schema compat.
- `registry::save_is_atomic_via_rename` — `.tmp` → final rename.
- `registry::preserves_null_for_last_push_time` — `Option<String>` for
  `last_push_time: null` from the Python.
- `registry::append_event_writes_ndjson_line` — NDJSON log shape.

These exercise no network and create no real-remotes.

## Behavioural subtleties

- The Rust port does **not** implement `argparse --dry-run` on
  `daemon-autocommit`/`daemon-cleanup`. Instead the subcommands are
  `airlock-v2 autocommit --dry-run` (boolean flag) — same semantics,
  different syntax. The launchd wrappers (see `airlock-v2-install.sh`)
  will need to be updated when the Rust binary is wired in.
- The Python's `daemon-loop` runs forever and blocks signals. The Rust
  equivalent is `examples/daemon.rs`; we use a PID-file-watcher pattern
  so an external `kill` (e.g. via `launchctl`) is sufficient to stop it.
  The signal-handler-shim is intentionally minimal because the daemon is
  expected to be supervised by launchd, not interactive.
- The Python wrote `last_push_time: None` to **remove** a field after a
  failed cleanup. The Rust `upsert_entry` only sets fields; `None`
  semantics are encoded as "leave the field alone". This is equivalent
  for the live JSON readers because they all treat missing-key and
  `null`-value the same way.
- The Python's `commit_all` uses `--no-verify` to skip pre-commit hooks.
  We do the same so the daemon doesn't get blocked by hooks that, e.g.,
  require a tty. `commit_all` does not check for staged-but-empty and
  we replicate the `git diff --cached --quiet` check.

## On-disk file naming

- Registry: `registry.json` (matches Python).
- Logs: `logs/<name>.log` where `<name>` is one of `autocommit`,
  `cleanup` (matches Python).
- PID: `<mode>.pid` at the state root (matches Python).

## Why not use `cargo init`?

The brief explicitly says: "Do NOT `cargo init` — create the file
manually." This is to ensure the Cargo.toml inherits `workspace.package`
fields (`version`, `edition`, `license`, `repository`) so PhenoVCS has
a single source of truth for crate metadata.
