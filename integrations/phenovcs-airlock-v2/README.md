# PhenoVCS Airlock v2 compatibility integration

This directory is a deliberately isolated Rust compatibility integration for
the Airlock fork. It is not part of the Go daemon's default build and does not
replace the canonical Airlock server.

## Provenance and attribution

The implementation is migrated from `KooshaPari/PhenoVCS`, crate
`crates/airlock-v2`, which in turn documents its Rust port of the historical
`.airlock/bin/airlock-v2.py` engine. The source PhenoVCS repository remains
unchanged and archive-only. Preserve this attribution when modifying or
redistributing the integration; repository history provides the full original
license and commit provenance.

## Compatibility boundary

- Build this crate from this directory with `cargo test --manifest-path
  Cargo.toml`; it is a standalone Cargo package with explicit metadata.
- It keeps the Airlock v2 CLI/state schema and conservative FF-only snapshot
  behavior, but it is not wired into the Go MCP daemon or release artifacts.
- The Go daemon and HTTP/MCP API remain the supported Airlock runtime. Adopt
  this Rust package only behind an explicit integration decision and parity
  tests; do not run both daemons against the same state directory by default.
- Any future integration must verify state-schema compatibility, lock/registry
  ownership, signal handling, and rollback before enabling it in CI or launchd.

---


Conservative auto-save / push daemon for git repositories. This crate
is a Rust port of the original `airlock-v2.py` engine (vendored from
`~/.airlock/bin/`). The two daemons share state at `~/.airlock/v2/` —
schema is byte-compatible.

## Why port?

The Python engine is single-threaded and has no structured logging. The
Rust port:

- Adds explicit types for `RepoEntry`, `AutocommitRecord`,
  `CleanupRecord`, `Registry` (with serde compatibility on both shapes).
- Keeps the same CLI surface (register/unregister/list/status/snapshot/
  autocommit/cleanup) but exposes the underlying cycles as library
  functions so callers can compose them.
- Replaces `argparse --dry-run` with a single boolean flag.
- Uses `std::process::Command` exclusively for git operations (matching
  `crates/worktree-manager`).
- Never calls `git push --force` or `git push --atomic --force-with-lease`.
  FF-only; on rejection, falls back to `wip/<date>-<uuid>` snapshot.

## Build

```sh
cargo build -p airlock-v2 --release
```

## Run

```sh
# Single-shot 15-minute autocommit pass:
target/release/airlock-v2 autocommit

# Single-shot 8-hour cleanup pass:
target/release/airlock-v2 cleanup --dry-run

# Register a repo:
target/release/airlock-v2 register /Users/kooshapari/CodeProjects/Phenotype/repos/PhenoVCS

# Audit every registered repo:
target/release/airlock-v2 audit

# Long-running scheduler (called by launchd):
cargo run --release --example daemon -- autocommit
```

## Layout

- `src/lib.rs` — `StateRoot`, intervals, error-free constructors.
- `src/registry.rs` — JSON read/write with schema compat.
- `src/git_ops.rs` — shell-out git helpers; **no `git2`**.
- `src/autocommit.rs` — 15-min cycle.
- `src/cleanup.rs` — 8-hr cycle.
- `src/cli.rs` — clap-based dispatch.
- `src/main.rs` — binary entry.
- `examples/daemon.rs` — long-running scheduler.
- `docs/ADAPTATION.md` — Python → Rust notes.

## Test

```sh
cargo test -p airlock-v2
```

Tests use `tempfile` to create ephemeral git repos; no network is
required.

## See also

- `docs/ADAPTATION.md` — full port-and-upgrade notes.
- `~/CodeProjects/Phenotype/repos/.airlock/bin/airlock-v2.py` — original
  Python engine.
