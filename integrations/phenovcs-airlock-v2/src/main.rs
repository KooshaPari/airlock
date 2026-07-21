//! `airlock-v2` binary entry point.
//!
//! The Rust port of `airlock-v2.py`. Reads/writes the same on-disk
//! registry as the live Python daemon (see `docs/ADAPTATION.md`).
//!
//! ## Usage
//!
//! ```text
//! airlock-v2 register    <repo-path>
//! airlock-v2 unregister  <repo-path>
//! airlock-v2 list
//! airlock-v2 status      <repo-path>
//! airlock-v2 snapshot    <repo-path> [-m note]
//! airlock-v2 autocommit  [--dry-run]
//! airlock-v2 cleanup     [--dry-run]
//! airlock-v2 daemon      <autocommit|cleanup>
//! airlock-v2 audit
//! airlock-v2 quickstatus
//! ```
//!
//! For a long-running scheduler, use the `daemon` example:
//! `cargo run --release --example daemon -- autocommit`.

use anyhow::Result;

use airlock_v2::cli::{run, Cli};
use clap::Parser;
use airlock_v2::StateRoot;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state_root = StateRoot::default_from_home();
    let code = run(&cli, &state_root)?;
    std::process::exit(code);
}
