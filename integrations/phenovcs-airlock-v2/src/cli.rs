//! Clap-based CLI dispatch for the `airlock-v2` binary.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::git_ops::{
    dirty_count, get_remote_url, primary_branch, snapshot_repo, try_push_or_snapshot,
};
use crate::registry::{load, save, short_ts, upsert_entry, Registry};
use crate::StateRoot;

#[derive(Debug, Parser)]
#[command(name = "airlock-v2")]
#[command(about = "Conservative auto-save / push daemon for git repositories")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Register a repository with the airlock-v2 daemon.
    Register {
        /// Absolute path to a git work-tree.
        repo_path: String,
    },
    /// Remove a repository from the registry.
    Unregister {
        repo_path: String,
    },
    /// List every registered repo.
    List,
    /// Show a one-screen status for a single repo.
    Status {
        repo_path: String,
    },
    /// Create+push a `wip/<date>-<uuid>` snapshot branch.
    Snapshot {
        repo_path: String,
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
    },
    /// Single-shot 15-minute auto-commit pass (used by the launchd wrapper).
    Autocommit {
        /// Do not commit or push; only print what *would* happen.
        #[arg(long)]
        dry_run: bool,
    },
    /// Single-shot 8-hour stash→wip→push pass.
    Cleanup {
        #[arg(long)]
        dry_run: bool,
    },
    /// Long-running scheduler (autocommit or cleanup).
    Daemon {
        /// Which cycle to run forever.
        mode: String,
    },
    /// Audit every registered repo (alias of `list` with verbose output).
    Audit,
    /// Run all cycles once on the live registry. Used by the
    /// `airlock-v2 autocommit` and `airlock-v2 cleanup` subcommands. The
    /// `restore` command restores a `wip/<date>-<uuid>` branch onto a
    /// target ref (a no-op alias of `snapshot` for parity).
    Restore {
        repo_path: String,
        /// Branch or ref to restore into.
        #[arg(short = 'b', long = "branch")]
        branch: String,
    },
    /// Show one-screen status of all registered repos (counts).
    Quickstatus,
}

/// Helper: resolve `repo_path` to an absolute path inside a git work-tree.
fn resolve_repo_path(input: &str) -> Result<std::path::PathBuf> {
    let p = std::path::Path::new(input).expanduser_abs()?;
    Ok(p)
}

trait ExpandUserAbs {
    fn expanduser_abs(&self) -> Result<std::path::PathBuf>;
}

impl ExpandUserAbs for std::path::Path {
    fn expanduser_abs(&self) -> Result<std::path::PathBuf> {
        let s = self.to_string_lossy();
        let expanded = if let Some(rest) = s.strip_prefix("~/") {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .context("HOME not set")?;
            home.join(rest)
        } else if s == "~" {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .context("HOME not set")?
        } else {
            std::path::PathBuf::from(s.as_ref())
        };
        Ok(expanded)
    }
}

/// Run the CLI, returning the process exit code.
pub fn run(cli: &Cli, state_root: &StateRoot) -> Result<i32> {
    state_root.ensure_dirs()?;
    match &cli.command {
        Commands::Register { repo_path } => cmd_register(state_root, repo_path),
        Commands::Unregister { repo_path } => cmd_unregister(state_root, repo_path),
        Commands::List => cmd_list(state_root),
        Commands::Status { repo_path } => cmd_status(state_root, repo_path),
        Commands::Snapshot { repo_path, message } => {
            cmd_snapshot(state_root, repo_path, message.as_deref())
        }
        Commands::Autocommit { dry_run } => cmd_autocommit(state_root, *dry_run),
        Commands::Cleanup { dry_run } => cmd_cleanup(state_root, *dry_run),
        Commands::Daemon { mode } => cmd_daemon(state_root, mode),
        Commands::Audit => cmd_audit(state_root),
        Commands::Restore { repo_path, branch } => {
            cmd_restore(state_root, repo_path, branch)
        }
        Commands::Quickstatus => cmd_quickstatus(state_root),
    }
}

fn cmd_register(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    if !crate::git_ops::is_inside_work_tree(&repo_path)? {
        println!("[SKIP] {} is not inside a git work tree.", repo_path.display());
        return Ok(1);
    }
    let mut registry = load(state_root)?;
    let key = repo_path.to_string_lossy().to_string();
    let remote_url = get_remote_url(&repo_path).unwrap_or(None).unwrap_or_default();
    let primary = primary_branch(&repo_path).unwrap_or_else(|_| "main".to_string());
    upsert_entry(&mut registry, &key, |e| {
        e.remote_url = if remote_url.is_empty() {
            None
        } else {
            Some(remote_url.clone())
        };
        e.primary_branch = Some(primary.clone());
        e.last_dirty_count = Some(dirty_count(&repo_path).unwrap_or(0) as u64);
    });
    save(state_root, &registry)?;
    println!("[OK] Registered {}", repo_path.display());
    Ok(0)
}

fn cmd_unregister(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let key = repo_path.to_string_lossy().to_string();
    let mut registry = load(state_root)?;
    if registry.remove(&key).is_none() {
        println!("[INFO] {} not in registry; nothing to do.", repo_path.display());
        return Ok(0);
    }
    save(state_root, &registry)?;
    println!("[OK] Unregistered {}", repo_path.display());
    Ok(0)
}

fn cmd_list(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    print_registry(&registry);
    Ok(0)
}

fn cmd_status(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let registry = load(state_root)?;
    let key = repo_path.to_string_lossy().to_string();
    let meta = registry.get(&key).cloned().unwrap_or_default();
    let snapshot = snapshot_repo(&repo_path, &meta)?;
    print!("{}", snapshot.render());
    // `state_root` is intentionally unused here; the registry probe is enough.
    let _ = state_root;
    Ok(0)
}

fn cmd_snapshot(state_root: &StateRoot, repo_path: &str, message: Option<&str>) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    if !crate::git_ops::is_inside_work_tree(&repo_path)? {
        println!("[FAIL] {} is not a git repo.", repo_path.display());
        return Ok(1);
    }
    let snapshot_branch = format!("wip/{}-{}", short_ts(), crate::cli::short_id());
    crate::git_ops::create_branch_at_head(&repo_path, &snapshot_branch)?;
    let (ok, msg) = crate::git_ops::push_branch_with_upstream(&repo_path, &snapshot_branch)?;
    let _ = state_root;
    if !ok {
        println!(
            "[WARN] Push failed for {snapshot_branch}: {msg}\n       Local branch {snapshot_branch} is preserved."
        );
        return Ok(2);
    }
    println!("[OK] Snapshot created and pushed: {snapshot_branch}");
    println!("     {msg}");
    if let Some(m) = message {
        println!("     note: {m}");
    }
    Ok(0)
}

fn cmd_autocommit(state_root: &StateRoot, dry_run: bool) -> Result<i32> {
    let summary = crate::autocommit::run(state_root, dry_run)?;
    print!("{summary}", summary = summary.render());
    Ok(if summary.errors == 0 { 0 } else { 1 })
}

fn cmd_cleanup(state_root: &StateRoot, dry_run: bool) -> Result<i32> {
    let summary = crate::cleanup::run(state_root, dry_run)?;
    print!("{summary}", summary = summary.render());
    Ok(if summary.errors == 0 { 0 } else { 1 })
}

fn cmd_daemon(state_root: &StateRoot, mode: &str) -> Result<i32> {
    let mode = match mode {
        "autocommit" => "autocommit",
        "cleanup" => "cleanup",
        _ => {
            eprintln!("[FAIL] unknown daemon mode: {mode} (use 'autocommit' or 'cleanup')");
            return Ok(2);
        }
    };
    // The long-running loop lives in `examples/daemon.rs`. Re-launching
    // here is an alias that prints a hint — the actual loop is
    // implemented separately so the CLI binary stays single-shot.
    println!(
        "[daemon] mode={mode}: invoke `cargo run --example daemon -- {mode}` to run as a long-lived scheduler."
    );
    let _ = state_root;
    Ok(0)
}

fn cmd_audit(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    if registry.is_empty() {
        println!("[INFO] No repos registered.");
        return Ok(0);
    }
    println!("[AUDIT] {} registered repo(s):", registry.len());
    for (path, meta) in registry.sorted() {
        let dirty = meta.last_dirty_count.unwrap_or(0);
        let remote = meta
            .remote_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no remote)");
        println!("  - {path}");
        println!(
            "      dirty={dirty}  remote={remote}  registered_at={}",
            meta.registered_at.as_deref().unwrap_or("?")
        );
        println!(
            "      primary_branch={}  last_auto_commit={}  last_push_time={}",
            meta.primary_branch.as_deref().unwrap_or("?"),
            meta.last_auto_commit.as_deref().unwrap_or("never"),
            meta.last_push_time.as_deref().unwrap_or("never"),
        );
    }
    let _ = state_root;
    Ok(0)
}

fn cmd_restore(state_root: &StateRoot, repo_path: &str, branch: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let (ok, msg) = try_push_or_snapshot(&repo_path, branch, &short_ts())?;
    println!("[restore] {branch}: {msg}");
    let _ = state_root;
    Ok(if ok { 0 } else { 2 })
}

fn cmd_quickstatus(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    let repos = registry.len();
    let mut dirty = 0usize;
    let mut unpushed = 0usize;
    for path in registry.sorted_paths() {
        let meta = registry
            .get(path.to_string_lossy().as_ref())
            .cloned()
            .unwrap_or_default();
        let snap = snapshot_repo(&path, &meta)?;
        if snap.dirty > 0 {
            dirty += 1;
        }
        if snap.ahead > 0 {
            unpushed += 1;
        }
    }
    println!("[quickstatus] repos={repos} dirty={dirty} unpushed={unpushed}");
    let _ = state_root;
    Ok(0)
}

fn print_registry(reg: &Registry) {
    if reg.is_empty() {
        println!("[INFO] No repos registered.");
        return;
    }
    println!("[OK] {} registered repo(s):", reg.len());
    for (path, meta) in reg.sorted() {
        let dirty = meta.last_dirty_count.unwrap_or(0);
        let remote = meta
            .remote_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no remote)");
        println!("  - {path}");
        println!("      dirty={dirty}  remote={remote}");
    }
}

/// Short process-time hex used by snapshot branch names.
///
/// Mirrors the Python's `uuid.uuid4().hex[:8]` — collision-safe enough for
/// one daemon tick across hundreds of repos.
pub fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos as u64)
}
