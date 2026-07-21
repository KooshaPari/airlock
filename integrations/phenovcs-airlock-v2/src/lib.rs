//! `airlock_v2` library — Rust port of `airlock-v2.py`.
//!
//! ## Architecture
//!
//! The crate is layered so that pure logic stays testable and the noisy
//! I/O (git invocations, registry files, PID files) is funneled through
//! small adapter modules. Nothing is global; all state is plumbed through
//! the [`StateRoot`] type so the crate can be exercised against a sandboxed
//! state directory (tests, dry-runs).
//!
//! ## State location
//!
//! The Python engine stores its state at `~/.airlock/v2/` (NOT
//! `~/.airlock-v2/registry/`). The Rust port **must** target the same
//! directory because the live daemon is still Python and we share one
//! registry on disk. See `docs/ADAPTATION.md` for the full note.
//!
//! ## Schema compatibility
//!
//! Registry JSON shape is byte-compatible with the Python daemon:
//!
//! ```jsonc
//! {
//!   "/abs/path/to/repo": {
//!     "registered_at":     "2026-07-17T01:22:33Z",
//!     "last_check":        "2026-07-17T01:22:33Z",
//!     "last_dirty_count":  0,
//!     "remote_url":        "git@github.com:...",
//!     "primary_branch":    "main",
//!     "last_auto_commit":  "2026-07-17T01:22:33Z",  // optional, written by autocommit
//!     "last_push_time":    "2026-07-17T01:22:33Z"   // optional, may be null
//!   }
//! }
//! ```
//!
//! Both daemons read/write this file; do not change field names without
//! co-bumping the Python registry. Log lines are NDJSON, one event per line.

pub mod autocommit;
pub mod cleanup;
pub mod cli;
pub mod git_ops;
pub mod registry;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default state root — `$HOME/.airlock/v2`.
///
/// Mirrors the Python engine: `STATE_ROOT = HOME / ".airlock" / "v2"`.
pub const DEFAULT_STATE_ROOT_SUFFIX: &str = ".airlock/v2";

/// Conservative intervals matching the Python defaults.
pub const AUTOCOMMIT_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(8 * 60 * 60);

/// Agent identity used by commits when no user-supplied env override is set.
pub const AGENT_AUTHOR_NAME: &str = "Airlock Bot";
pub const AGENT_AUTHOR_EMAIL: &str = "airlock@phenoforge.local";

/// Layout of the on-disk state directory.
///
/// Created lazily by [`StateRoot::ensure_dirs`].
#[derive(Debug, Clone)]
pub struct StateRoot {
    root: PathBuf,
}

impl StateRoot {
    /// Construct a state root from any path (does not touch the filesystem).
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }

    /// Derive the default state root from `$HOME`.
    pub fn default_from_home() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let suffix = Path::new(DEFAULT_STATE_ROOT_SUFFIX);
        Self::from_path(home.join(suffix))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registry_path(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn log_path(&self, name: &str) -> PathBuf {
        self.logs_dir().join(format!("{name}.log"))
    }

    /// Recursively ensure every directory the engine might write to exists.
    ///
    /// This intentionally never deletes anything — even on a corrupted
    /// layout, we only `mkdir -p`.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.root())?;
        std::fs::create_dir_all(self.state_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        Ok(())
    }
}

impl Default for StateRoot {
    fn default() -> Self {
        Self::default_from_home()
    }
}
